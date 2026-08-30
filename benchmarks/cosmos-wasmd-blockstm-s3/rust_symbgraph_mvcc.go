package main

import (
	"bytes"
	"sort"
	"sync"
	"sync/atomic"

	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
)

// rustVisibilityMask is the Go/Wasmd analogue of runtime/crates/
// acg-cosmwasm-engine::VisibilityMask. It freezes, at transaction launch, the
// successful earlier-canonical speculative versions that are visible for the
// lifetime of that transaction.
type rustVisibilityMask struct {
	words []uint64
}

func rustVisibilityFromCompleted(completed []uint64, canonicalIndex int) rustVisibilityMask {
	if canonicalIndex <= 0 {
		return rustVisibilityMask{}
	}
	required := (canonicalIndex + 63) / 64
	if required > len(completed) {
		required = len(completed)
	}
	words := append([]uint64(nil), completed[:required]...)
	if canonicalIndex%64 != 0 && len(words) != 0 {
		words[len(words)-1] &= (uint64(1) << uint(canonicalIndex%64)) - 1
	}
	return rustVisibilityMask{words: words}
}

func (m rustVisibilityMask) contains(index int) bool {
	if index < 0 {
		return false
	}
	word := index / 64
	bit := uint(index % 64)
	return word < len(m.words) && m.words[word]&(uint64(1)<<bit) != 0
}

func markRustCompleted(words []uint64, index int) {
	if index < 0 || index/64 >= len(words) {
		return
	}
	words[index/64] |= uint64(1) << uint(index%64)
}

type rustBytesVersion struct {
	transaction int
	deleted     bool
	value       []byte
}

type rustObjectVersion struct {
	transaction int
	deleted     bool
	value       any
}

type rustMVCCStoreVersions struct {
	bytes   map[string][]rustBytesVersion
	objects map[string][]rustObjectVersion
}

type rustMVCCDiagnostics struct {
	pointReads    atomic.Uint64
	versionHits   atomic.Uint64
	baseFallbacks atomic.Uint64
	rangeReads    atomic.Uint64
	rangeKeys     atomic.Uint64
	publishes     atomic.Uint64
	publishedKeys atomic.Uint64
}

type rustMVCCSnapshot struct {
	PointReads    uint64
	VersionHits   uint64
	BaseFallbacks uint64
	RangeReads    uint64
	RangeKeys     uint64
	Publishes     uint64
	PublishedKeys uint64
}

// rustBlockMVCC is shared by all speculative transactions in one block. It
// contains only completed successful speculative deltas; the block-start
// Cosmos MultiStore remains the immutable fallback.
type rustBlockMVCC struct {
	mu     sync.RWMutex
	stores map[storetypes.StoreKey]*rustMVCCStoreVersions
	diag   rustMVCCDiagnostics
}

func newRustBlockMVCC() *rustBlockMVCC {
	return &rustBlockMVCC{stores: make(map[storetypes.StoreKey]*rustMVCCStoreVersions, 8)}
}

func (m *rustBlockMVCC) diagnostics() rustMVCCSnapshot {
	if m == nil {
		return rustMVCCSnapshot{}
	}
	return rustMVCCSnapshot{
		PointReads:    m.diag.pointReads.Load(),
		VersionHits:   m.diag.versionHits.Load(),
		BaseFallbacks: m.diag.baseFallbacks.Load(),
		RangeReads:    m.diag.rangeReads.Load(),
		RangeKeys:     m.diag.rangeKeys.Load(),
		Publishes:     m.diag.publishes.Load(),
		PublishedKeys: m.diag.publishedKeys.Load(),
	}
}

func (m *rustBlockMVCC) publish(transaction int, delta rustTxDelta) {
	if m == nil || len(delta.entries) == 0 {
		return
	}
	m.mu.Lock()
	for _, entry := range delta.entries {
		versions := m.stores[entry.storeKey]
		if versions == nil {
			versions = &rustMVCCStoreVersions{
				bytes:   make(map[string][]rustBytesVersion),
				objects: make(map[string][]rustObjectVersion),
			}
			m.stores[entry.storeKey] = versions
		}
		key := string(entry.key)
		if entry.object {
			versions.objects[key] = append(versions.objects[key], rustObjectVersion{
				transaction: transaction,
				deleted:     entry.deleted,
				value:       entry.value,
			})
		} else {
			versions.bytes[key] = append(versions.bytes[key], rustBytesVersion{
				transaction: transaction,
				deleted:     entry.deleted,
				value:       cloneBytes(entry.bytes),
			})
		}
	}
	m.mu.Unlock()
	m.diag.publishes.Add(1)
	m.diag.publishedKeys.Add(uint64(len(delta.entries)))
}

func newestRustBytesVersion(versions []rustBytesVersion, reader int, visibility rustVisibilityMask) (rustBytesVersion, bool) {
	best := -1
	var out rustBytesVersion
	for _, version := range versions {
		if version.transaction >= reader || !visibility.contains(version.transaction) || version.transaction <= best {
			continue
		}
		best = version.transaction
		out = version
	}
	return out, best >= 0
}

func newestRustObjectVersion(versions []rustObjectVersion, reader int, visibility rustVisibilityMask) (rustObjectVersion, bool) {
	best := -1
	var out rustObjectVersion
	for _, version := range versions {
		if version.transaction >= reader || !visibility.contains(version.transaction) || version.transaction <= best {
			continue
		}
		best = version.transaction
		out = version
	}
	return out, best >= 0
}

func (m *rustBlockMVCC) bytesValue(storeKey storetypes.StoreKey, key []byte, reader int, visibility rustVisibilityMask) ([]byte, bool, bool) {
	if m == nil {
		return nil, false, false
	}
	m.diag.pointReads.Add(1)
	m.mu.RLock()
	store := m.stores[storeKey]
	if store == nil {
		m.mu.RUnlock()
		m.diag.baseFallbacks.Add(1)
		return nil, false, false
	}
	version, ok := newestRustBytesVersion(store.bytes[string(key)], reader, visibility)
	m.mu.RUnlock()
	if !ok {
		m.diag.baseFallbacks.Add(1)
		return nil, false, false
	}
	m.diag.versionHits.Add(1)
	return cloneBytes(version.value), version.deleted, true
}

func (m *rustBlockMVCC) objectValue(storeKey storetypes.StoreKey, key []byte, reader int, visibility rustVisibilityMask) (any, bool, bool) {
	if m == nil {
		return nil, false, false
	}
	m.diag.pointReads.Add(1)
	m.mu.RLock()
	store := m.stores[storeKey]
	if store == nil {
		m.mu.RUnlock()
		m.diag.baseFallbacks.Add(1)
		return nil, false, false
	}
	version, ok := newestRustObjectVersion(store.objects[string(key)], reader, visibility)
	m.mu.RUnlock()
	if !ok {
		m.diag.baseFallbacks.Add(1)
		return nil, false, false
	}
	m.diag.versionHits.Add(1)
	return version.value, version.deleted, true
}

type rustBytesMutation struct {
	deleted bool
	value   []byte
}

type rustObjectMutation struct {
	deleted bool
	value   any
}

func (m *rustBlockMVCC) bytesRange(storeKey storetypes.StoreKey, start, end []byte, reader int, visibility rustVisibilityMask) map[string]rustBytesMutation {
	out := make(map[string]rustBytesMutation)
	if m == nil {
		return out
	}
	m.diag.rangeReads.Add(1)
	m.mu.RLock()
	store := m.stores[storeKey]
	if store != nil {
		for raw, versions := range store.bytes {
			key := []byte(raw)
			if !keyInRange(key, start, end) {
				continue
			}
			if version, ok := newestRustBytesVersion(versions, reader, visibility); ok {
				out[raw] = rustBytesMutation{deleted: version.deleted, value: cloneBytes(version.value)}
			}
		}
	}
	m.mu.RUnlock()
	m.diag.rangeKeys.Add(uint64(len(out)))
	return out
}

func (m *rustBlockMVCC) objectRange(storeKey storetypes.StoreKey, start, end []byte, reader int, visibility rustVisibilityMask) map[string]rustObjectMutation {
	out := make(map[string]rustObjectMutation)
	if m == nil {
		return out
	}
	m.diag.rangeReads.Add(1)
	m.mu.RLock()
	store := m.stores[storeKey]
	if store != nil {
		for raw, versions := range store.objects {
			key := []byte(raw)
			if !keyInRange(key, start, end) {
				continue
			}
			if version, ok := newestRustObjectVersion(versions, reader, visibility); ok {
				out[raw] = rustObjectMutation{deleted: version.deleted, value: version.value}
			}
		}
	}
	m.mu.RUnlock()
	m.diag.rangeKeys.Add(uint64(len(out)))
	return out
}

// rustLocalOverlay mirrors the nested Cosmos CacheMultiStore hierarchy only for
// transaction-local mutations. It lets reads distinguish a local Set/Delete
// from an unchanged delegate value before consulting block MVCC.
type rustLocalOverlay struct {
	parent  *rustLocalOverlay
	bytes   map[storetypes.StoreKey]map[string]rustBytesMutation
	objects map[storetypes.StoreKey]map[string]rustObjectMutation
}

func newRustLocalOverlay(parent *rustLocalOverlay) *rustLocalOverlay {
	return &rustLocalOverlay{
		parent:  parent,
		bytes:   make(map[storetypes.StoreKey]map[string]rustBytesMutation),
		objects: make(map[storetypes.StoreKey]map[string]rustObjectMutation),
	}
}

func (o *rustLocalOverlay) setBytes(storeKey storetypes.StoreKey, key, value []byte) {
	if o == nil {
		return
	}
	store := o.bytes[storeKey]
	if store == nil {
		store = make(map[string]rustBytesMutation)
		o.bytes[storeKey] = store
	}
	store[string(key)] = rustBytesMutation{value: cloneBytes(value)}
}

func (o *rustLocalOverlay) deleteBytes(storeKey storetypes.StoreKey, key []byte) {
	if o == nil {
		return
	}
	store := o.bytes[storeKey]
	if store == nil {
		store = make(map[string]rustBytesMutation)
		o.bytes[storeKey] = store
	}
	store[string(key)] = rustBytesMutation{deleted: true}
}

func (o *rustLocalOverlay) setObject(storeKey storetypes.StoreKey, key []byte, value any) {
	if o == nil {
		return
	}
	store := o.objects[storeKey]
	if store == nil {
		store = make(map[string]rustObjectMutation)
		o.objects[storeKey] = store
	}
	store[string(key)] = rustObjectMutation{value: value}
}

func (o *rustLocalOverlay) deleteObject(storeKey storetypes.StoreKey, key []byte) {
	if o == nil {
		return
	}
	store := o.objects[storeKey]
	if store == nil {
		store = make(map[string]rustObjectMutation)
		o.objects[storeKey] = store
	}
	store[string(key)] = rustObjectMutation{deleted: true}
}

func (o *rustLocalOverlay) lookupBytes(storeKey storetypes.StoreKey, key []byte) (rustBytesMutation, bool) {
	for current := o; current != nil; current = current.parent {
		if store := current.bytes[storeKey]; store != nil {
			if mutation, ok := store[string(key)]; ok {
				return mutation, true
			}
		}
	}
	return rustBytesMutation{}, false
}

func (o *rustLocalOverlay) lookupObject(storeKey storetypes.StoreKey, key []byte) (rustObjectMutation, bool) {
	for current := o; current != nil; current = current.parent {
		if store := current.objects[storeKey]; store != nil {
			if mutation, ok := store[string(key)]; ok {
				return mutation, true
			}
		}
	}
	return rustObjectMutation{}, false
}

func (o *rustLocalOverlay) chainRootFirst() []*rustLocalOverlay {
	var chain []*rustLocalOverlay
	for current := o; current != nil; current = current.parent {
		chain = append(chain, current)
	}
	for i, j := 0, len(chain)-1; i < j; i, j = i+1, j-1 {
		chain[i], chain[j] = chain[j], chain[i]
	}
	return chain
}

func (o *rustLocalOverlay) mergeIntoParent() {
	if o == nil || o.parent == nil {
		return
	}
	for storeKey, entries := range o.bytes {
		for raw, mutation := range entries {
			if mutation.deleted {
				o.parent.deleteBytes(storeKey, []byte(raw))
			} else {
				o.parent.setBytes(storeKey, []byte(raw), mutation.value)
			}
		}
	}
	for storeKey, entries := range o.objects {
		for raw, mutation := range entries {
			if mutation.deleted {
				o.parent.deleteObject(storeKey, []byte(raw))
			} else {
				o.parent.setObject(storeKey, []byte(raw), mutation.value)
			}
		}
	}
}

type rustMvccReadView struct {
	versions       *rustBlockMVCC
	canonicalIndex int
	visibility     rustVisibilityMask
}

func (v *rustMvccReadView) bytesValue(storeKey storetypes.StoreKey, key []byte) ([]byte, bool, bool) {
	if v == nil {
		return nil, false, false
	}
	return v.versions.bytesValue(storeKey, key, v.canonicalIndex, v.visibility)
}

func (v *rustMvccReadView) objectValue(storeKey storetypes.StoreKey, key []byte) (any, bool, bool) {
	if v == nil {
		return nil, false, false
	}
	return v.versions.objectValue(storeKey, key, v.canonicalIndex, v.visibility)
}

func (v *rustMvccReadView) bytesRange(storeKey storetypes.StoreKey, start, end []byte) map[string]rustBytesMutation {
	if v == nil {
		return nil
	}
	return v.versions.bytesRange(storeKey, start, end, v.canonicalIndex, v.visibility)
}

func (v *rustMvccReadView) objectRange(storeKey storetypes.StoreKey, start, end []byte) map[string]rustObjectMutation {
	if v == nil {
		return nil
	}
	return v.versions.objectRange(storeKey, start, end, v.canonicalIndex, v.visibility)
}

type rustSliceIterator[V any] struct {
	start  []byte
	end    []byte
	keys   [][]byte
	values []V
	index  int
	closed bool
	err    error
}

func (it *rustSliceIterator[V]) Domain() ([]byte, []byte) { return it.start, it.end }
func (it *rustSliceIterator[V]) Valid() bool {
	return !it.closed && it.index >= 0 && it.index < len(it.keys)
}
func (it *rustSliceIterator[V]) Next() {
	if !it.Valid() {
		panic("Next called on invalid Rust MVCC iterator")
	}
	it.index++
}
func (it *rustSliceIterator[V]) Key() []byte {
	if !it.Valid() {
		panic("Key called on invalid Rust MVCC iterator")
	}
	return it.keys[it.index]
}
func (it *rustSliceIterator[V]) Value() V {
	if !it.Valid() {
		panic("Value called on invalid Rust MVCC iterator")
	}
	return it.values[it.index]
}
func (it *rustSliceIterator[V]) Error() error { return it.err }
func (it *rustSliceIterator[V]) Close() error {
	it.closed = true
	return nil
}

func collectByteIterator(it storetypes.Iterator) (map[string][]byte, error) {
	defer it.Close()
	out := make(map[string][]byte)
	for ; it.Valid(); it.Next() {
		out[string(it.Key())] = cloneBytes(it.Value())
	}
	return out, it.Error()
}

func collectObjectIterator(it storetypes.ObjIterator) (map[string]any, error) {
	defer it.Close()
	out := make(map[string]any)
	for ; it.Valid(); it.Next() {
		out[string(it.Key())] = it.Value()
	}
	return out, it.Error()
}

func rustByteErrorIterator(start, end []byte, err error) storetypes.Iterator {
	return &rustSliceIterator[[]byte]{start: cloneBytes(start), end: cloneBytes(end), err: err}
}

func rustObjectErrorIterator(start, end []byte, err error) storetypes.ObjIterator {
	return &rustSliceIterator[any]{start: cloneBytes(start), end: cloneBytes(end), err: err}
}

func rustByteIterator(start, end []byte, values map[string][]byte, reverse bool) storetypes.Iterator {
	keys := make([][]byte, 0, len(values))
	for raw := range values {
		keys = append(keys, []byte(raw))
	}
	sort.Slice(keys, func(i, j int) bool { return bytes.Compare(keys[i], keys[j]) < 0 })
	if reverse {
		for i, j := 0, len(keys)-1; i < j; i, j = i+1, j-1 {
			keys[i], keys[j] = keys[j], keys[i]
		}
	}
	items := make([][]byte, len(keys))
	for i, key := range keys {
		items[i] = cloneBytes(values[string(key)])
	}
	return &rustSliceIterator[[]byte]{start: cloneBytes(start), end: cloneBytes(end), keys: keys, values: items}
}

func rustObjectIterator(start, end []byte, values map[string]any, reverse bool) storetypes.ObjIterator {
	keys := make([][]byte, 0, len(values))
	for raw := range values {
		keys = append(keys, []byte(raw))
	}
	sort.Slice(keys, func(i, j int) bool { return bytes.Compare(keys[i], keys[j]) < 0 })
	if reverse {
		for i, j := 0, len(keys)-1; i < j; i, j = i+1, j-1 {
			keys[i], keys[j] = keys[j], keys[i]
		}
	}
	items := make([]any, len(keys))
	for i, key := range keys {
		items[i] = values[string(key)]
	}
	return &rustSliceIterator[any]{start: cloneBytes(start), end: cloneBytes(end), keys: keys, values: items}
}
