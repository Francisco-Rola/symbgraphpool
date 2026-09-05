package main

import (
	"bytes"
	"context"
	"fmt"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	abci "github.com/cometbft/cometbft/abci/types"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

// policyRunStats records concrete strategy work. Attempts counts executions in the
// measured phase; Reexecutions counts executions beyond each strategy's initial batch.
type policyRunStats struct {
	Attempts                          uint64
	Reexecutions                      uint64
	Speculated                        uint64
	Reused                            uint64
	Replayed                          uint64
	PreConsensusNanos                 uint64
	PostConsensusNanos                uint64
	ValidationNanos                   uint64
	ReplayExecutionNanos              uint64
	ConflictAnalysisNanos             uint64
	DiscoveredConflicts               uint64
	ForwardFallbacks                  uint64
	SafetyReplays                     uint64
	SnapshotBuildNanos                uint64
	SnapshotPointHits                 uint64
	SnapshotPointMisses               uint64
	SnapshotRangeHits                 uint64
	SnapshotRangeMisses               uint64
	PostBatches                       uint64
	PostSingletonBatches              uint64
	PostMaxBatch                      uint64
	ReadySelectionNanos               uint64
	PreExecWorkNanos                  uint64
	PreExecSpanNanos                  uint64
	PostExecWorkNanos                 uint64
	PostExecSpanNanos                 uint64
	PostWideExecWorkNanos             uint64
	PostWideExecSpanNanos             uint64
	PostWideTransactions              uint64
	VegetaLongestChain                uint64
	VegetaChainCount                  uint64
	VegetaWeightedLongestChainCost    uint64
	VegetaTotalEstimatedCost          uint64
	VegetaHotKeyWorkerLowerBoundCost  uint64
	VegetaReadyWorkerLowerBoundCost   uint64
	VegetaAlg3ValidationNanos         uint64
	VegetaRangeValidationNanos        uint64
	VegetaIntrinsicReexecutionNanos   uint64
	AriaInitialBatchNanos             uint64
	AriaInitialExecWorkNanos          uint64
	AriaAcceptedCommitNanos           uint64
	AriaFallbackDAGBuildNanos         uint64
	AriaFallbackBranchBuildNanos      uint64
	AriaFallbackVisibilityNanos       uint64
	AriaFallbackMVCCPublishNanos      uint64
	AriaFallbackPublishedDeltaEntries uint64
	AriaFallbackTxExecWorkNanos       uint64
	AriaFallbackFinalCommitNanos      uint64
}

// ariaSemanticExecutionError marks a source-successful transaction that cannot
// execute under Aria's speculative/derived serialization state. This is not an
// internal scheduler failure: on a fixed historical blockchain stream it means
// the alternative serialization is not safe to carry into subsequent blocks.
// The caller may discard the staged Aria block and commit historical order.
type ariaSemanticExecutionError struct {
	phase string
	index int
	log   string
}

func (e *ariaSemanticExecutionError) Error() string {
	return fmt.Sprintf("aria-fb %s tx %d failed: %s", e.phase, e.index, e.log)
}

// vegetaSemanticExecutionError marks a source-successful transaction that cannot
// execute under Vegeta's speculative/derived serialization state. The caller
// must discard the staged Vegeta branch and replay historical block order from
// the unchanged block-start state. Historical replay remains fail-closed.
type vegetaSemanticExecutionError struct {
	phase string
	index int
	log   string
}

func (e *vegetaSemanticExecutionError) Error() string {
	return fmt.Sprintf("vegeta %s tx %d failed: %s", e.phase, e.index, e.log)
}

type storeID uint64
type accessID uint64

// writeLocation keeps raw bytes only for unique writes. Exact read/write
// validation uses accessID and never materializes a string. Raw write keys are
// retained because iterator/range reads must still compare real ordered keys.
type writeLocation struct {
	store storeID
	key   []byte
}

type writeSet struct {
	exact      map[accessID]writeLocation
	collisions map[accessID][]writeLocation
}

type storeRange struct {
	store storeID
	start []byte
	end   []byte
}

const (
	fnvOffset64 = uint64(14695981039346656037)
	fnvPrime64  = uint64(1099511628211)
)

// Different stores hashing to the same numeric ID are safe: they can only
// create conservative false conflicts, never hide a same-store conflict.
func hashBytes(seed uint64, bz []byte) uint64 {
	h := seed
	for _, b := range bz {
		h ^= uint64(b)
		h *= fnvPrime64
	}
	return h
}

func storeIDFromName(name string) storeID {
	h := fnvOffset64
	for i := 0; i < len(name); i++ {
		h ^= uint64(name[i])
		h *= fnvPrime64
	}
	return storeID(h)
}

type storeIDRegistry struct {
	ids  map[storetypes.StoreKey]storeID
	keys map[storeID]storetypes.StoreKey
}

func newStoreIDRegistry() *storeIDRegistry {
	return &storeIDRegistry{
		ids:  make(map[storetypes.StoreKey]storeID, 16),
		keys: make(map[storeID]storetypes.StoreKey, 16),
	}
}

func (r *storeIDRegistry) id(key storetypes.StoreKey) storeID {
	if id, ok := r.ids[key]; ok {
		return id
	}
	id := storeIDFromName(key.Name())
	r.ids[key] = id
	// Hash collisions are conservatively treated as one store by access validation.
	// Keep the first concrete StoreKey so transaction-local deltas can be captured.
	if _, exists := r.keys[id]; !exists {
		r.keys[id] = key
	}
	return id
}

func (r *storeIDRegistry) key(id storeID) (storetypes.StoreKey, bool) {
	key, ok := r.keys[id]
	return key, ok
}

func (r *storeIDRegistry) merge(other *storeIDRegistry) {
	if r == nil || other == nil {
		return
	}
	for key, id := range other.ids {
		if _, ok := r.ids[key]; !ok {
			r.ids[key] = id
		}
	}
	for id, key := range other.keys {
		if _, ok := r.keys[id]; !ok {
			r.keys[id] = key
		}
	}
}

func exactAccessID(store storeID, key []byte) accessID {
	// Mix the store ID bytewise into FNV state, then hash the raw KV key. This
	// stays allocation-free on the hot Get/Has/Set/Delete path.
	h := fnvOffset64
	s := uint64(store)
	for i := 0; i < 8; i++ {
		h ^= s & 0xff
		h *= fnvPrime64
		s >>= 8
	}
	return accessID(hashBytes(h, key))
}

func cloneBytes(bz []byte) []byte {
	if bz == nil {
		return nil
	}
	// make+copy preserves the distinction between nil and a valid,
	// zero-length value. append([]byte(nil), bz...) collapses []byte{}
	// to nil, which makes store/v2 reject the value when a captured delta
	// is materialized with KVStore.Set.
	out := make([]byte, len(bz))
	copy(out, bz)
	return out
}

func sameWriteLocation(a writeLocation, store storeID, key []byte) bool {
	return a.store == store && bytes.Equal(a.key, key)
}

func newWriteSet(capacity int) writeSet {
	return writeSet{exact: make(map[accessID]writeLocation, capacity)}
}

func (s *writeSet) add(store storeID, key []byte) {
	id := exactAccessID(store, key)
	if existing, ok := s.exact[id]; ok {
		if sameWriteLocation(existing, store, key) {
			return
		}
		// A true 64-bit collision is extremely rare. Preserve every raw key so
		// iterator/range validation remains exact and collisions stay conservative.
		for _, c := range s.collisions[id] {
			if sameWriteLocation(c, store, key) {
				return
			}
		}
		if s.collisions == nil {
			s.collisions = make(map[accessID][]writeLocation)
		}
		s.collisions[id] = append(s.collisions[id], writeLocation{store: store, key: cloneBytes(key)})
		return
	}
	s.exact[id] = writeLocation{store: store, key: cloneBytes(key)}
}

func (s *writeSet) addLocation(id accessID, loc writeLocation) {
	if existing, ok := s.exact[id]; ok {
		if sameWriteLocation(existing, loc.store, loc.key) {
			return
		}
		for _, c := range s.collisions[id] {
			if sameWriteLocation(c, loc.store, loc.key) {
				return
			}
		}
		if s.collisions == nil {
			s.collisions = make(map[accessID][]writeLocation)
		}
		s.collisions[id] = append(s.collisions[id], loc)
		return
	}
	s.exact[id] = loc
}

func (s *writeSet) merge(other writeSet) {
	for id, loc := range other.exact {
		s.addLocation(id, loc)
	}
	for id, collisions := range other.collisions {
		for _, loc := range collisions {
			s.addLocation(id, loc)
		}
	}
}

// accessTracker is transaction-local and deliberately lock-free. Exact reads
// are stored only as 64-bit fingerprints. Unique writes retain their raw bytes
// solely for exact iterator/range validation.
type accessTracker struct {
	reads               map[accessID]struct{}
	readLocations       writeSet
	retainReadLocations bool
	ranges              []storeRange
	writes              writeSet
	parent              *accessTracker
}

func newAccessTracker(parent *accessTracker) *accessTracker {
	retain := parent != nil && parent.retainReadLocations
	return &accessTracker{
		reads:               make(map[accessID]struct{}, 64),
		readLocations:       newWriteSet(64),
		retainReadLocations: retain,
		writes:              newWriteSet(16),
		parent:              parent,
	}
}

func (t *accessTracker) read(store storeID, key []byte) {
	id := exactAccessID(store, key)
	t.reads[id] = struct{}{}
	if t.retainReadLocations {
		t.readLocations.addLocation(id, writeLocation{store: store, key: cloneBytes(key)})
	}
	// A discarded nested CacheContext can still influence control flow. Bubble
	// the concrete read location up immediately; writes only bubble when that
	// cache is committed. Keeping the raw point-read key is block-local and lets
	// Vegeta materialize a lock-free post-consensus read snapshot.
	if t.parent != nil {
		t.parent.read(store, key)
	}
}

func (t *accessTracker) readID(id accessID) {
	t.reads[id] = struct{}{}
	if t.parent != nil {
		t.parent.readID(id)
	}
}

func (t *accessTracker) readRange(store storeID, start, end []byte) {
	t.ranges = append(t.ranges, storeRange{store: store, start: cloneBytes(start), end: cloneBytes(end)})
	if t.parent != nil {
		t.parent.readRange(store, start, end)
	}
}

func (t *accessTracker) write(store storeID, key []byte) {
	t.writes.add(store, key)
}

func (t *accessTracker) mergeIntoParent() {
	if t.parent == nil {
		return
	}
	t.parent.writes.merge(t.writes)
}

func keyInRange(key, start, end []byte) bool {
	if start != nil && bytes.Compare(key, start) < 0 {
		return false
	}
	if end != nil && bytes.Compare(key, end) >= 0 {
		return false
	}
	return true
}

func readsConflictWithWrites(reads *accessTracker, writes *writeSet) bool {
	for id, loc := range writes.exact {
		if _, ok := reads.reads[id]; ok {
			return true
		}
		for _, r := range reads.ranges {
			if r.store == loc.store && keyInRange(loc.key, r.start, r.end) {
				return true
			}
		}
	}
	for id, collisions := range writes.collisions {
		// Any read with the colliding fingerprint is conservatively conflicting.
		if _, ok := reads.reads[id]; ok {
			return true
		}
		for _, loc := range collisions {
			for _, r := range reads.ranges {
				if r.store == loc.store && keyInRange(loc.key, r.start, r.end) {
					return true
				}
			}
		}
	}
	return false
}

func mergeWrites(dst *writeSet, tracker *accessTracker) {
	dst.merge(tracker.writes)
}

// vegetaReadSnapshot is an immutable, batch-start view of point/range reads
// discovered during Vegeta pre-consensus execution. Ordinary Cosmos cachekv
// stores intentionally serialize access through a mutex because their IAVL
// parent is not concurrency-safe. Serving known post-consensus reads from this
// block-local snapshot keeps parallel workers off that shared parent lock.
//
// A post-consensus access that was not seen during pre-execution simply misses
// this snapshot and falls through to the ordinary SDK store. Vegeta's existing
// access-change/new-key validation then decides whether that transaction can be
// reused or must be replayed, so the optimization cannot hide a dynamic access.
type vegetaRangeKey struct {
	store    storetypes.StoreKey
	start    string
	end      string
	startNil bool
	endNil   bool
}

type vegetaReadSnapshot struct {
	bytes       map[storetypes.StoreKey]map[string]rustBytesMutation
	byteRanges  map[vegetaRangeKey]map[string]rustBytesMutation
	pointHits   atomic.Uint64
	pointMisses atomic.Uint64
	rangeHits   atomic.Uint64
	rangeMisses atomic.Uint64
}

func newVegetaReadSnapshot() *vegetaReadSnapshot {
	return &vegetaReadSnapshot{
		bytes:      make(map[storetypes.StoreKey]map[string]rustBytesMutation),
		byteRanges: make(map[vegetaRangeKey]map[string]rustBytesMutation),
	}
}

func vegetaSnapshotRangeKey(store storetypes.StoreKey, start, end []byte) vegetaRangeKey {
	return vegetaRangeKey{store: store, start: string(start), end: string(end), startNil: start == nil, endNil: end == nil}
}

func (s *vegetaReadSnapshot) setBytes(store storetypes.StoreKey, key, value []byte) {
	if s == nil {
		return
	}
	entries := s.bytes[store]
	if entries == nil {
		entries = make(map[string]rustBytesMutation)
		s.bytes[store] = entries
	}
	entries[string(key)] = rustBytesMutation{deleted: value == nil, value: cloneBytes(value)}
}

func (s *vegetaReadSnapshot) bytesValue(store storetypes.StoreKey, key []byte) ([]byte, bool, bool) {
	if s == nil {
		return nil, false, false
	}
	mutation, ok := s.bytes[store][string(key)]
	if !ok {
		s.pointMisses.Add(1)
		return nil, false, false
	}
	s.pointHits.Add(1)
	return mutation.value, mutation.deleted, true
}

func (s *vegetaReadSnapshot) setBytesRange(store storetypes.StoreKey, start, end []byte, values map[string]rustBytesMutation) {
	if s == nil {
		return
	}
	s.byteRanges[vegetaSnapshotRangeKey(store, start, end)] = values
}

func (s *vegetaReadSnapshot) bytesRange(store storetypes.StoreKey, start, end []byte) (map[string]rustBytesMutation, bool) {
	if s == nil {
		return nil, false
	}
	stored, ok := s.byteRanges[vegetaSnapshotRangeKey(store, start, end)]
	if !ok {
		s.rangeMisses.Add(1)
		return nil, false
	}
	s.rangeHits.Add(1)
	out := make(map[string]rustBytesMutation, len(stored))
	for raw, mutation := range stored {
		out[raw] = rustBytesMutation{deleted: mutation.deleted, value: cloneBytes(mutation.value)}
	}
	return out, true
}

type vegetaSnapshotStats struct {
	PointHits   uint64
	PointMisses uint64
	RangeHits   uint64
	RangeMisses uint64
}

func (s *vegetaReadSnapshot) stats() vegetaSnapshotStats {
	if s == nil {
		return vegetaSnapshotStats{}
	}
	return vegetaSnapshotStats{
		PointHits: s.pointHits.Load(), PointMisses: s.pointMisses.Load(),
		RangeHits: s.rangeHits.Load(), RangeMisses: s.rangeMisses.Load(),
	}
}

func buildVegetaReadSnapshot(ms storetypes.MultiStore, indices []int, trackers []*accessTracker, stores *storeIDRegistry) (*vegetaReadSnapshot, error) {
	snapshot := newVegetaReadSnapshot()
	base := ms
	if tracked, ok := ms.(*trackingMultiStore); ok {
		base = tracked.cacheMultiStoreDelegate.CacheMultiStore
	}
	seenPoints := make(map[accessID]struct{})
	seenRanges := make(map[vegetaRangeKey]struct{})
	for _, idx := range indices {
		if idx < 0 || idx >= len(trackers) || trackers[idx] == nil {
			continue
		}
		tracker := trackers[idx]
		forEachWrite(&tracker.readLocations, func(id accessID, loc writeLocation) {
			if _, ok := seenPoints[id]; ok {
				return
			}
			storeKey, ok := stores.key(loc.store)
			if !ok {
				return
			}
			store := base.GetStore(storeKey)
			kv, ok := store.(storetypes.KVStore)
			if !ok {
				return
			}
			snapshot.setBytes(storeKey, loc.key, kv.Get(loc.key))
			seenPoints[id] = struct{}{}
		})
		for _, readRange := range tracker.ranges {
			storeKey, ok := stores.key(readRange.store)
			if !ok {
				continue
			}
			rangeKey := vegetaSnapshotRangeKey(storeKey, readRange.start, readRange.end)
			if _, ok := seenRanges[rangeKey]; ok {
				continue
			}
			store := base.GetStore(storeKey)
			kv, ok := store.(storetypes.KVStore)
			if !ok {
				continue
			}
			it := kv.Iterator(readRange.start, readRange.end)
			values, err := collectByteIterator(it)
			if err != nil {
				return nil, err
			}
			captured := make(map[string]rustBytesMutation, len(values))
			for raw, value := range values {
				captured[raw] = rustBytesMutation{value: cloneBytes(value)}
			}
			snapshot.setBytesRange(storeKey, readRange.start, readRange.end, captured)
			seenRanges[rangeKey] = struct{}{}
		}
	}
	return snapshot, nil
}

// trackingKVStore records actual Wasmd/Cosmos accesses while delegating storage
// semantics to the SDK cache store itself.
type trackingKVStore struct {
	storetypes.KVStore
	storeKey storetypes.StoreKey
	store    storeID
	tracker  *accessTracker
	readView *rustMvccReadView
	snapshot *vegetaReadSnapshot
	overlay  *rustLocalOverlay
}

func (s trackingKVStore) Get(key []byte) []byte {
	s.tracker.read(s.store, key)
	if mutation, ok := s.overlay.lookupBytes(s.storeKey, key); ok {
		if mutation.deleted {
			return nil
		}
		return cloneBytes(mutation.value)
	}
	if value, deleted, ok := s.snapshot.bytesValue(s.storeKey, key); ok {
		if deleted {
			return nil
		}
		return value
	}
	if value, deleted, ok := s.readView.bytesValue(s.storeKey, key); ok {
		if deleted {
			return nil
		}
		return value
	}
	return s.KVStore.Get(key)
}
func (s trackingKVStore) Has(key []byte) bool {
	s.tracker.read(s.store, key)
	if mutation, ok := s.overlay.lookupBytes(s.storeKey, key); ok {
		return !mutation.deleted
	}
	if _, deleted, ok := s.snapshot.bytesValue(s.storeKey, key); ok {
		return !deleted
	}
	if _, deleted, ok := s.readView.bytesValue(s.storeKey, key); ok {
		return !deleted
	}
	return s.KVStore.Has(key)
}
func (s trackingKVStore) Set(key, value []byte) {
	s.tracker.write(s.store, key)
	s.overlay.setBytes(s.storeKey, key, value)
	s.KVStore.Set(key, value)
}
func (s trackingKVStore) Delete(key []byte) {
	s.tracker.write(s.store, key)
	s.overlay.deleteBytes(s.storeKey, key)
	s.KVStore.Delete(key)
}
func (s trackingKVStore) iterator(start, end []byte, reverse bool) storetypes.Iterator {
	s.tracker.readRange(s.store, start, end)
	snapshotValues, covered := s.snapshot.bytesRange(s.storeKey, start, end)
	values := make(map[string][]byte, len(snapshotValues))
	if covered {
		for raw, mutation := range snapshotValues {
			if !mutation.deleted {
				values[raw] = cloneBytes(mutation.value)
			}
		}
	} else {
		var underlying storetypes.Iterator
		if reverse {
			underlying = s.KVStore.ReverseIterator(start, end)
		} else {
			underlying = s.KVStore.Iterator(start, end)
		}
		var err error
		values, err = collectByteIterator(underlying)
		if err != nil {
			return rustByteErrorIterator(start, end, err)
		}
	}
	for raw, mutation := range s.readView.bytesRange(s.storeKey, start, end) {
		if mutation.deleted {
			delete(values, raw)
		} else {
			values[raw] = cloneBytes(mutation.value)
		}
	}
	for _, layer := range s.overlay.chainRootFirst() {
		for raw, mutation := range layer.bytes[s.storeKey] {
			key := []byte(raw)
			if !keyInRange(key, start, end) {
				continue
			}
			if mutation.deleted {
				delete(values, raw)
			} else {
				values[raw] = cloneBytes(mutation.value)
			}
		}
	}
	return rustByteIterator(start, end, values, reverse)
}
func (s trackingKVStore) Iterator(start, end []byte) storetypes.Iterator {
	return s.iterator(start, end, false)
}
func (s trackingKVStore) ReverseIterator(start, end []byte) storetypes.Iterator {
	return s.iterator(start, end, true)
}

// ObjKVStore is generic over any in store/v2. Track it as well so scheduler
// correctness does not depend on a keeper choosing byte-backed vs object stores.
type trackingObjKVStore struct {
	storetypes.ObjKVStore
	storeKey storetypes.StoreKey
	store    storeID
	tracker  *accessTracker
	readView *rustMvccReadView
	overlay  *rustLocalOverlay
}

func (s trackingObjKVStore) Get(key []byte) any {
	s.tracker.read(s.store, key)
	if mutation, ok := s.overlay.lookupObject(s.storeKey, key); ok {
		if mutation.deleted {
			return nil
		}
		return mutation.value
	}
	if value, deleted, ok := s.readView.objectValue(s.storeKey, key); ok {
		if deleted {
			return nil
		}
		return value
	}
	return s.ObjKVStore.Get(key)
}
func (s trackingObjKVStore) Has(key []byte) bool {
	s.tracker.read(s.store, key)
	if mutation, ok := s.overlay.lookupObject(s.storeKey, key); ok {
		return !mutation.deleted
	}
	if _, deleted, ok := s.readView.objectValue(s.storeKey, key); ok {
		return !deleted
	}
	return s.ObjKVStore.Has(key)
}
func (s trackingObjKVStore) Set(key []byte, value any) {
	s.tracker.write(s.store, key)
	s.overlay.setObject(s.storeKey, key, value)
	s.ObjKVStore.Set(key, value)
}
func (s trackingObjKVStore) Delete(key []byte) {
	s.tracker.write(s.store, key)
	s.overlay.deleteObject(s.storeKey, key)
	s.ObjKVStore.Delete(key)
}
func (s trackingObjKVStore) iterator(start, end []byte, reverse bool) storetypes.ObjIterator {
	s.tracker.readRange(s.store, start, end)
	var underlying storetypes.ObjIterator
	if reverse {
		underlying = s.ObjKVStore.ReverseIterator(start, end)
	} else {
		underlying = s.ObjKVStore.Iterator(start, end)
	}
	values, err := collectObjectIterator(underlying)
	if err != nil {
		return rustObjectErrorIterator(start, end, err)
	}
	for raw, mutation := range s.readView.objectRange(s.storeKey, start, end) {
		if mutation.deleted {
			delete(values, raw)
		} else {
			values[raw] = mutation.value
		}
	}
	for _, layer := range s.overlay.chainRootFirst() {
		for raw, mutation := range layer.objects[s.storeKey] {
			key := []byte(raw)
			if !keyInRange(key, start, end) {
				continue
			}
			if mutation.deleted {
				delete(values, raw)
			} else {
				values[raw] = mutation.value
			}
		}
	}
	return rustObjectIterator(start, end, values, reverse)
}
func (s trackingObjKVStore) Iterator(start, end []byte) storetypes.ObjIterator {
	return s.iterator(start, end, false)
}
func (s trackingObjKVStore) ReverseIterator(start, end []byte) storetypes.ObjIterator {
	return s.iterator(start, end, true)
}

// cacheMultiStoreDelegate deliberately wraps CacheMultiStore one level down
// instead of embedding storetypes.CacheMultiStore directly in trackingMultiStore.
// CacheMultiStore is both an embedded field name and a required MultiStore method;
// direct embedding therefore collides with trackingMultiStore.CacheMultiStore().
type cacheMultiStoreDelegate struct {
	storetypes.CacheMultiStore
}

type trackingMultiStore struct {
	cacheMultiStoreDelegate
	tracker              *accessTracker
	stores               *storeIDRegistry
	readView             *rustMvccReadView
	snapshot             *vegetaReadSnapshot
	overlay              *rustLocalOverlay
	commitTracker        *accessTracker
	commitStores         *storeIDRegistry
	speculativeStoreKeys []storetypes.StoreKey
}

func wrapTrackingCacheMultiStore(store storetypes.CacheMultiStore, parent *accessTracker, stores *storeIDRegistry, readView *rustMvccReadView, parentOverlay *rustLocalOverlay) *trackingMultiStore {
	// The local overlay is required only when an external Rust MVCC read view is
	// active. Plain Cosmos CacheMultiStore branches already provide exact
	// read-your-writes semantics, so maintaining a second overlay for Vegeta,
	// Aria and safety-reference execution only duplicates every mutation (and
	// clones byte values) on their hottest path.
	var overlay *rustLocalOverlay
	if readView != nil || parentOverlay != nil {
		overlay = newRustLocalOverlay(parentOverlay)
	}
	return &trackingMultiStore{
		cacheMultiStoreDelegate: cacheMultiStoreDelegate{CacheMultiStore: store},
		tracker:                 newAccessTracker(parent),
		stores:                  stores,
		readView:                readView,
		overlay:                 overlay,
	}
}

func newTrackingMultiStore(parent storetypes.MultiStore) *trackingMultiStore {
	return wrapTrackingCacheMultiStore(parent.CacheMultiStore(), nil, newStoreIDRegistry(), nil, nil)
}

func newTrackingMultiStoreForSpeculation(parent storetypes.MultiStore, storeKeys []storetypes.StoreKey) *trackingMultiStore {
	tracked := newTrackingMultiStore(parent)
	tracked.speculativeStoreKeys = append([]storetypes.StoreKey(nil), storeKeys...)
	return tracked
}

func newTrackingMultiStoreWithMVCC(parent storetypes.MultiStore, readView *rustMvccReadView) *trackingMultiStore {
	return wrapTrackingCacheMultiStore(parent.CacheMultiStore(), nil, newStoreIDRegistry(), readView, nil)
}

// newSpeculativeTrackingMultiStoreWithMVCC is the transaction-local variant used
// when the parent is itself a staged tracking store. It preserves the same
// private-tracker rule as newSpeculativeTrackingMultiStore while serving
// predecessor values from a concurrent block-local MVCC view instead of
// physically copying every ancestor delta into each child branch.
func newSpeculativeTrackingMultiStoreWithMVCC(parent storetypes.MultiStore, readView *rustMvccReadView) *trackingMultiStore {
	tracked := newSpeculativeTrackingMultiStore(parent)
	tracked.readView = readView
	if tracked.overlay == nil {
		tracked.overlay = newRustLocalOverlay(nil)
	}
	return tracked
}

// newSpeculativeTrackingMultiStore creates a transaction-local branch without
// stacking tracking wrappers when the parent is already a tracking store. Aria's
// canonical block staging intentionally passes a *trackingMultiStore into the
// scheduler. Calling parent.CacheMultiStore() through the public wrapper would
// create an inner tracker whose reads bubble into the shared block tracker while
// sibling speculative transactions are running concurrently. Besides racing on
// the tracker's maps, that would pollute the staged block with accesses from
// speculative transactions that may later be discarded.
//
// Instead branch directly from the parent's underlying CacheMultiStore. The
// transaction gets private access/store registries; accepted writes are merged
// into the staged block tracker only when Write() commits that transaction.
func newSpeculativeTrackingMultiStore(parent storetypes.MultiStore) *trackingMultiStore {
	tracked, ok := parent.(*trackingMultiStore)
	if !ok {
		return newTrackingMultiStore(parent)
	}
	child := tracked.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStore()
	var overlay *rustLocalOverlay
	if tracked.readView != nil || tracked.overlay != nil {
		overlay = newRustLocalOverlay(tracked.overlay)
	}
	return &trackingMultiStore{
		cacheMultiStoreDelegate: cacheMultiStoreDelegate{CacheMultiStore: child},
		tracker:                 newAccessTracker(nil),
		stores:                  newStoreIDRegistry(),
		readView:                tracked.readView,
		overlay:                 overlay,
		commitTracker:           tracked.tracker,
		commitStores:            tracked.stores,
		speculativeStoreKeys:    tracked.speculativeStoreKeys,
	}
}

func newSpeculativeTrackingMultiStoreWithSnapshot(parent storetypes.MultiStore, snapshot *vegetaReadSnapshot) *trackingMultiStore {
	tracked := newSpeculativeTrackingMultiStore(parent)
	tracked.snapshot = snapshot
	// Snapshot reads bypass the delegate, so retain a transaction-local overlay
	// to preserve read-your-writes before consulting the immutable batch view.
	if tracked.overlay == nil {
		tracked.overlay = newRustLocalOverlay(nil)
	}
	return tracked
}

// prewarmSpeculativeParent initializes the SDK cachemulti.Store wrappers for
// every mounted store before any sibling speculative branch executes. store/v2
// lazily inserts wrappers into an internal map from GetStore/GetKVStore; allowing
// multiple transaction goroutines to trigger that initialization through a
// shared staged parent causes concurrent map writes inside cachemulti.Store.
//
// Warm-up is deliberately outside the measured parallel execution and happens
// once per speculative phase. After it returns, sibling child caches only read
// the parent's wrapper map while maintaining their own private cache/tracker.
func prewarmSpeculativeParent(ms storetypes.MultiStore) {
	tracked, ok := ms.(*trackingMultiStore)
	if !ok || len(tracked.speculativeStoreKeys) == 0 {
		return
	}
	parent := tracked.cacheMultiStoreDelegate.CacheMultiStore
	for _, key := range tracked.speculativeStoreKeys {
		_ = parent.GetStore(key)
	}
}

func (m *trackingMultiStore) CacheWrap() storetypes.CacheWrap { return m.CacheMultiStore() }

func (m *trackingMultiStore) CacheMultiStore() storetypes.CacheMultiStore {
	child := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStore()
	wrapped := wrapTrackingCacheMultiStore(child, m.tracker, m.stores, m.readView, m.overlay)
	wrapped.snapshot = m.snapshot
	return wrapped
}

func (m *trackingMultiStore) CacheMultiStoreWithVersion(version int64) (storetypes.CacheMultiStore, error) {
	child, err := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStoreWithVersion(version)
	if err != nil {
		return nil, err
	}
	wrapped := wrapTrackingCacheMultiStore(child, m.tracker, m.stores, m.readView, m.overlay)
	wrapped.snapshot = m.snapshot
	return wrapped, nil
}

func (m *trackingMultiStore) GetStore(key storetypes.StoreKey) storetypes.Store {
	store := m.cacheMultiStoreDelegate.CacheMultiStore.GetStore(key)
	if kv, ok := store.(storetypes.KVStore); ok {
		return trackingKVStore{KVStore: kv, storeKey: key, store: m.stores.id(key), tracker: m.tracker, readView: m.readView, snapshot: m.snapshot, overlay: m.overlay}
	}
	if obj, ok := store.(storetypes.ObjKVStore); ok {
		return trackingObjKVStore{ObjKVStore: obj, storeKey: key, store: m.stores.id(key), tracker: m.tracker, readView: m.readView, overlay: m.overlay}
	}
	return store
}

func (m *trackingMultiStore) GetKVStore(key storetypes.StoreKey) storetypes.KVStore {
	return trackingKVStore{
		KVStore:  m.cacheMultiStoreDelegate.CacheMultiStore.GetKVStore(key),
		storeKey: key,
		store:    m.stores.id(key),
		tracker:  m.tracker,
		readView: m.readView,
		snapshot: m.snapshot,
		overlay:  m.overlay,
	}
}

func (m *trackingMultiStore) GetObjKVStore(key storetypes.StoreKey) storetypes.ObjKVStore {
	return trackingObjKVStore{
		ObjKVStore: m.cacheMultiStoreDelegate.CacheMultiStore.GetObjKVStore(key),
		storeKey:   key,
		store:      m.stores.id(key),
		tracker:    m.tracker,
		readView:   m.readView,
		overlay:    m.overlay,
	}
}

func (m *trackingMultiStore) Write() {
	m.cacheMultiStoreDelegate.CacheMultiStore.Write()
	m.tracker.mergeIntoParent()
	if m.commitTracker != nil {
		m.commitTracker.writes.merge(m.tracker.writes)
	}
	if m.commitStores != nil {
		m.commitStores.merge(m.stores)
	}
	m.overlay.mergeIntoParent()
}

// trackingBranchesFinalStateEqual compares the final values of every key
// written by either staged block branch. Both branches have the same immutable
// block-start parent, so equality on the union of writes is sufficient to prove
// identical post-block KV state without committing either branch.
func trackingBranchesFinalStateEqual(left, right *trackingMultiStore) bool {
	if left == nil || right == nil {
		return left == right
	}
	equal := true
	compare := func(source *trackingMultiStore, loc writeLocation) {
		if !equal {
			return
		}
		key, ok := source.stores.key(loc.store)
		if !ok {
			equal = false
			return
		}
		lv := left.cacheMultiStoreDelegate.CacheMultiStore.GetKVStore(key).Get(loc.key)
		rv := right.cacheMultiStoreDelegate.CacheMultiStore.GetKVStore(key).Get(loc.key)
		if !bytes.Equal(lv, rv) {
			equal = false
		}
	}
	forEachWrite(&left.tracker.writes, func(_ accessID, loc writeLocation) { compare(left, loc) })
	forEachWrite(&right.tracker.writes, func(_ accessID, loc writeLocation) { compare(right, loc) })
	return equal
}

type speculativeResult struct {
	index          int
	store          *trackingMultiStore
	result         *abci.ExecTxResult
	attempt        uint64
	execNanos      uint64
	execStartNanos int64
	execEndNanos   int64
}

// speculativeAccessResult is the pre-consensus form of speculativeResult.
// Vegeta's proposal/dependency phase needs only the observed access tracker and
// store-ID registry; retaining the transaction CacheMultiStore after execution
// keeps a second copy of every speculative write alive for no semantic benefit.
// Dropping the cache branch as soon as its tracker has been extracted reduces
// allocation lifetime and GC pressure on large Wasmd blocks.
type speculativeAccessResult struct {
	index          int
	tracker        *accessTracker
	stores         *storeIDRegistry
	result         *abci.ExecTxResult
	execNanos      uint64
	execStartNanos int64
	execEndNanos   int64
}

func prepareSpeculationWithSnapshot(ms storetypes.MultiStore, indices []int, snapshot *vegetaReadSnapshot, captureReadLocations bool) map[int]*trackingMultiStore {
	prewarmSpeculativeParent(ms)
	out := make(map[int]*trackingMultiStore, len(indices))
	// Branch creation is intentionally serialized. Execution on the independent
	// branches is parallel; this avoids depending on CacheMultiStore branch
	// construction itself being thread-safe. If ms is already tracked (for example
	// Aria's staged canonical block), fork below the tracking wrapper so sibling
	// transactions never share a mutable accessTracker during speculation.
	for _, idx := range indices {
		if snapshot != nil {
			out[idx] = newSpeculativeTrackingMultiStoreWithSnapshot(ms, snapshot)
		} else {
			out[idx] = newSpeculativeTrackingMultiStore(ms)
		}
		out[idx].tracker.retainReadLocations = captureReadLocations
	}
	return out
}

func prepareSpeculation(ms storetypes.MultiStore, indices []int) map[int]*trackingMultiStore {
	return prepareSpeculationWithSnapshot(ms, indices, nil, false)
}

func speculateIndicesWithSnapshot(
	ctx context.Context,
	workers int,
	ms storetypes.MultiStore,
	txs [][]byte,
	indices []int,
	deliverTx sdk.DeliverTxFunc,
	snapshot *vegetaReadSnapshot,
	captureReadLocations bool,
) map[int]speculativeResult {
	if workers < 1 {
		workers = 1
	}
	if len(indices) == 0 {
		return map[int]speculativeResult{}
	}
	// More than half of the observed Vegeta post-consensus ready waves are
	// singletons. Avoid a channel, producer goroutine and worker goroutine when
	// there is no parallel work to schedule.
	if len(indices) == 1 {
		prewarmSpeculativeParent(ms)
		idx := indices[0]
		var branch *trackingMultiStore
		if snapshot != nil {
			branch = newSpeculativeTrackingMultiStoreWithSnapshot(ms, snapshot)
		} else {
			branch = newSpeculativeTrackingMultiStore(ms)
		}
		branch.tracker.retainReadLocations = captureReadLocations
		execStarted := time.Now()
		res := deliverTx(txs[idx], nil, branch, idx, map[string]any{})
		execEnded := time.Now()
		return map[int]speculativeResult{idx: {index: idx, store: branch, result: res, attempt: 1, execNanos: uint64(execEnded.Sub(execStarted).Nanoseconds()), execStartNanos: execStarted.UnixNano(), execEndNanos: execEnded.UnixNano()}}
	}
	branches := prepareSpeculationWithSnapshot(ms, indices, snapshot, captureReadLocations)
	jobs := make(chan int)
	results := make(chan speculativeResult, len(indices))
	var wg sync.WaitGroup
	n := workers
	if n > len(indices) {
		n = len(indices)
	}
	for w := 0; w < n; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for idx := range jobs {
				select {
				case <-ctx.Done():
					return
				default:
				}
				branch := branches[idx]
				execStarted := time.Now()
				res := deliverTx(txs[idx], nil, branch, idx, map[string]any{})
				execEnded := time.Now()
				results <- speculativeResult{index: idx, store: branch, result: res, attempt: 1, execNanos: uint64(execEnded.Sub(execStarted).Nanoseconds()), execStartNanos: execStarted.UnixNano(), execEndNanos: execEnded.UnixNano()}
			}
		}()
	}
	go func() {
		for _, idx := range indices {
			jobs <- idx
		}
		close(jobs)
		wg.Wait()
		close(results)
	}()
	out := make(map[int]speculativeResult, len(indices))
	for r := range results {
		out[r.index] = r
	}
	return out
}

// speculateAccesses executes Vegeta's pre-consensus discovery pass without
// retaining transaction cache branches after execution. Branch construction
// stays serialized, matching the existing CacheMultiStore safety rule. Once all
// branches exist, ownership is streamed to workers and removed from the branch
// table as soon as it is dispatched, so completed pre-consensus cache branches
// can be reclaimed before the whole block finishes.
func speculateAccesses(
	ctx context.Context,
	workers int,
	ms storetypes.MultiStore,
	txs [][]byte,
	indices []int,
	deliverTx sdk.DeliverTxFunc,
) map[int]speculativeAccessResult {
	if workers < 1 {
		workers = 1
	}
	out := make(map[int]speculativeAccessResult, len(indices))
	if len(indices) == 0 {
		return out
	}
	branches := prepareSpeculationWithSnapshot(ms, indices, nil, true)

	type job struct {
		index int
		store *trackingMultiStore
	}
	jobs := make(chan job)
	resultBuffer := workers * 2
	if resultBuffer > len(indices) {
		resultBuffer = len(indices)
	}
	results := make(chan speculativeAccessResult, resultBuffer)
	var wg sync.WaitGroup
	n := workers
	if n > len(indices) {
		n = len(indices)
	}
	for w := 0; w < n; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for work := range jobs {
				select {
				case <-ctx.Done():
					return
				default:
				}
				branch := work.store
				execStarted := time.Now()
				res := deliverTx(txs[work.index], nil, branch, work.index, map[string]any{})
				execEnded := time.Now()
				results <- speculativeAccessResult{index: work.index, tracker: branch.tracker, stores: branch.stores, result: res, execNanos: uint64(execEnded.Sub(execStarted).Nanoseconds()), execStartNanos: execStarted.UnixNano(), execEndNanos: execEnded.UnixNano()}
			}
		}()
	}
	go func() {
		defer func() {
			close(jobs)
			wg.Wait()
			close(results)
		}()
		for _, idx := range indices {
			branch := branches[idx]
			delete(branches, idx)
			select {
			case jobs <- job{index: idx, store: branch}:
			case <-ctx.Done():
				return
			}
		}
	}()
	for result := range results {
		out[result.index] = result
	}
	return out
}

func speculateIndices(
	ctx context.Context,
	workers int,
	ms storetypes.MultiStore,
	txs [][]byte,
	indices []int,
	deliverTx sdk.DeliverTxFunc,
) map[int]speculativeResult {
	return speculateIndicesWithSnapshot(ctx, workers, ms, txs, indices, deliverTx, nil, false)
}

func replayOne(ms storetypes.MultiStore, tx []byte, idx int, deliverTx sdk.DeliverTxFunc) speculativeResult {
	branch := newSpeculativeTrackingMultiStore(ms)
	execStarted := time.Now()
	res := deliverTx(tx, nil, branch, idx, map[string]any{})
	execEnded := time.Now()
	return speculativeResult{index: idx, store: branch, result: res, attempt: 1, execNanos: uint64(execEnded.Sub(execStarted).Nanoseconds()), execStartNanos: execStarted.UnixNano(), execEndNanos: execEnded.UnixNano()}
}

// commitSpeculation preserves block serial order. A speculative result can be
// reused iff none of its actual reads observed a key/range changed by an earlier
// transaction after the speculative snapshot. Otherwise the transaction is
// replayed against the up-to-date parent store.
func commitSpeculation(
	ms storetypes.MultiStore,
	txs [][]byte,
	indices []int,
	spec map[int]speculativeResult,
	deliverTx sdk.DeliverTxFunc,
	results []*abci.ExecTxResult,
	priorWrites *writeSet,
	attempts *atomic.Uint64,
	stats *policyRunStats,
) error {
	return commitSpeculationWithForcedReplay(ms, txs, indices, spec, deliverTx, results, priorWrites, attempts, stats, nil)
}

func commitSpeculationWithForcedReplay(
	ms storetypes.MultiStore,
	txs [][]byte,
	indices []int,
	spec map[int]speculativeResult,
	deliverTx sdk.DeliverTxFunc,
	results []*abci.ExecTxResult,
	priorWrites *writeSet,
	attempts *atomic.Uint64,
	stats *policyRunStats,
	forcedReplay map[int]struct{},
) error {
	sort.Ints(indices)
	for _, idx := range indices {
		r, ok := spec[idx]
		if !ok {
			return fmt.Errorf("missing speculative result tx=%d", idx)
		}
		attempts.Add(1)
		if r.result != nil && r.result.Code != 0 {
			return fmt.Errorf("speculative tx %d failed: %s", idx, r.result.Log)
		}
		_, force := forcedReplay[idx]
		validationStarted := time.Now()
		invalid := force || readsConflictWithWrites(r.store.tracker, priorWrites)
		stats.ValidationNanos += uint64(time.Since(validationStarted).Nanoseconds())
		if invalid {
			stats.Replayed++
			replayStarted := time.Now()
			r = replayOne(ms, txs[idx], idx, deliverTx)
			stats.ReplayExecutionNanos += uint64(time.Since(replayStarted).Nanoseconds())
			attempts.Add(1)
			if r.result != nil && r.result.Code != 0 {
				return fmt.Errorf("replayed tx %d failed: %s", idx, r.result.Log)
			}
		} else {
			stats.Reused++
		}
		r.store.Write()
		mergeWrites(priorWrites, r.store.tracker)
		results[idx] = r.result
	}
	return nil
}

func writeSetsConflict(left, right *writeSet) bool {
	if left == nil || right == nil {
		return false
	}
	for id := range left.exact {
		if _, ok := right.exact[id]; ok {
			return true
		}
		if _, ok := right.collisions[id]; ok {
			return true
		}
	}
	for id := range left.collisions {
		if _, ok := right.exact[id]; ok {
			return true
		}
		if _, ok := right.collisions[id]; ok {
			return true
		}
	}
	return false
}

type dependencyKinds uint8

const (
	dependencyWAW dependencyKinds = 1 << iota
	dependencyRAW
	dependencyWAR
)

func dependencyBetween(earlier, later *accessTracker) dependencyKinds {
	if earlier == nil || later == nil {
		return 0
	}
	var kinds dependencyKinds
	if writeSetsConflict(&earlier.writes, &later.writes) {
		kinds |= dependencyWAW
	}
	if readsConflictWithWrites(later, &earlier.writes) {
		kinds |= dependencyRAW
	}
	if readsConflictWithWrites(earlier, &later.writes) {
		kinds |= dependencyWAR
	}
	return kinds
}

// vegetaDependencyBetween ports Algorithm 1 BuildDAG exactly at the
// dependency-class level. WAW takes precedence. If the same pair has both WAR
// and RAW relations, Algorithm 1 promotes that pair to WAW (lines 30-36); this
// stronger class is required because Rule 2 must not admit the pair in one
// replay batch. A one-directional pair remains WAR or RAW respectively.
func vegetaDependencyBetween(earlier, later *accessTracker) dependencyKinds {
	kinds := dependencyBetween(earlier, later)
	switch {
	case kinds&dependencyWAW != 0:
		return dependencyWAW
	case kinds&dependencyRAW != 0 && kinds&dependencyWAR != 0:
		return dependencyWAW
	case kinds&dependencyWAR != 0:
		return dependencyWAR
	case kinds&dependencyRAW != 0:
		return dependencyRAW
	default:
		return 0
	}
}

func writeSetHasID(writes *writeSet, id accessID) bool {
	if writes == nil {
		return false
	}
	if _, ok := writes.exact[id]; ok {
		return true
	}
	_, ok := writes.collisions[id]
	return ok
}

func writeSetContainsLocation(writes *writeSet, id accessID, loc writeLocation) bool {
	if writes == nil {
		return false
	}
	if existing, ok := writes.exact[id]; ok && sameWriteLocation(existing, loc.store, loc.key) {
		return true
	}
	for _, collision := range writes.collisions[id] {
		if sameWriteLocation(collision, loc.store, loc.key) {
			return true
		}
	}
	return false
}

func forEachWrite(writes *writeSet, fn func(accessID, writeLocation)) {
	if writes == nil {
		return
	}
	for id, loc := range writes.exact {
		fn(id, loc)
	}
	for id, collisions := range writes.collisions {
		for _, loc := range collisions {
			fn(id, loc)
		}
	}
}

func trackerTouchesID(tracker *accessTracker, id accessID) bool {
	if tracker == nil {
		return false
	}
	if _, ok := tracker.reads[id]; ok {
		return true
	}
	return writeSetHasID(&tracker.writes, id)
}

func writeSetsEqual(left, right *writeSet) bool {
	if left == nil || right == nil {
		return left == nil && right == nil
	}
	leftCount := 0
	rightCount := 0
	equal := true
	forEachWrite(left, func(id accessID, loc writeLocation) {
		leftCount++
		if !writeSetContainsLocation(right, id, loc) {
			equal = false
		}
	})
	forEachWrite(right, func(_ accessID, _ writeLocation) {
		rightCount++
	})
	return equal && leftCount == rightCount
}

func storeRangesEqual(left, right []storeRange) bool {
	if len(left) != len(right) {
		return false
	}
	used := make([]bool, len(right))
	for _, want := range left {
		matched := false
		for i, got := range right {
			if used[i] || want.store != got.store || !bytes.Equal(want.start, got.start) || !bytes.Equal(want.end, got.end) {
				continue
			}
			used[i] = true
			matched = true
			break
		}
		if !matched {
			return false
		}
	}
	return true
}

func accessTrackersEqual(left, right *accessTracker) bool {
	if left == nil || right == nil {
		return left == nil && right == nil
	}
	if len(left.reads) != len(right.reads) {
		return false
	}
	for id := range left.reads {
		if _, ok := right.reads[id]; !ok {
			return false
		}
	}
	return storeRangesEqual(left.ranges, right.ranges) && writeSetsEqual(&left.writes, &right.writes)
}

// hottestAccessChain mirrors Vegeta's findMostFrequentKey/findLongestChain idea:
// count each exact key at most once per transaction and return the transactions
// touching the most frequently accessed key in the supplied deterministic order.
// Cosmos iterator ranges have no direct Ethereum equivalent, so they participate
// in conflict validation but not in the hot-key reorder heuristic.
func hottestAccessChain(indices []int, trackers []*accessTracker) []int {
	counts := make(map[accessID]int)
	for _, idx := range indices {
		if idx < 0 || idx >= len(trackers) || trackers[idx] == nil {
			continue
		}
		seen := make(map[accessID]struct{}, len(trackers[idx].reads)+len(trackers[idx].writes.exact))
		for id := range trackers[idx].reads {
			seen[id] = struct{}{}
		}
		for id := range trackers[idx].writes.exact {
			seen[id] = struct{}{}
		}
		for id := range trackers[idx].writes.collisions {
			seen[id] = struct{}{}
		}
		for id := range seen {
			counts[id]++
		}
	}
	var hot accessID
	maxCount := 0
	found := false
	for id, count := range counts {
		if !found || count > maxCount || (count == maxCount && id < hot) {
			hot = id
			maxCount = count
			found = true
		}
	}
	if !found || maxCount == 0 {
		return nil
	}
	chain := make([]int, 0, maxCount)
	for _, idx := range indices {
		if idx >= 0 && idx < len(trackers) && trackerTouchesID(trackers[idx], hot) {
			chain = append(chain, idx)
		}
	}
	return chain
}

type vegetaAccessChain struct {
	id  accessID
	txs []int
}

// vegetaSortedDependencyChains ports Algorithm 1 SortDependencyChains from
// Vegeta. Every exact key touched in speculation contributes one chain; chains
// are sorted longest-first and transactions retain proposal-order position
// inside each chain. A transaction can occur in multiple chains. Ties are
// broken by access fingerprint solely to make the otherwise-unspecified tie
// deterministic across Go map iteration and repeated benchmark runs.
//
// Iterator/range dependencies remain in conflict validation but are not turned
// into point-key chains because the Vegeta paper defines chains over accessed
// keys. This makes the heuristic conservative with respect to Wasmd ranges
// without inventing an Ethereum-incompatible chain notion.
func vegetaSortedDependencyChains(indices []int, trackers []*accessTracker) []vegetaAccessChain {
	chains := make(map[accessID][]int)
	for _, idx := range indices {
		if idx < 0 || idx >= len(trackers) || trackers[idx] == nil {
			continue
		}
		tracker := trackers[idx]
		seen := make(map[accessID]struct{}, len(tracker.reads)+len(tracker.writes.exact)+len(tracker.writes.collisions))
		for id := range tracker.reads {
			seen[id] = struct{}{}
		}
		for id := range tracker.writes.exact {
			seen[id] = struct{}{}
		}
		for id := range tracker.writes.collisions {
			seen[id] = struct{}{}
		}
		for id := range seen {
			chains[id] = append(chains[id], idx)
		}
	}
	out := make([]vegetaAccessChain, 0, len(chains))
	for id, txs := range chains {
		out = append(out, vegetaAccessChain{id: id, txs: txs})
	}
	sort.Slice(out, func(i, j int) bool {
		if len(out[i].txs) != len(out[j].txs) {
			return len(out[i].txs) > len(out[j].txs)
		}
		return out[i].id < out[j].id
	})
	return out
}

func vegetaCostAt(costs []uint32, idx int) uint64 {
	if idx >= 0 && idx < len(costs) && costs[idx] > 0 {
		return uint64(costs[idx])
	}
	return 1
}

func vegetaProposalOrderWithParallelismStats(trackers []*accessTracker, costs []uint32, workers int) ([]int, int, int, uint64, uint64, uint64) {
	indices := make([]int, len(trackers))
	for i := range indices {
		indices[i] = i
	}
	chains := vegetaSortedDependencyChains(indices, trackers)
	proposal := make([]int, 0, len(indices))
	included := make(map[int]struct{}, len(indices))
	for _, chain := range chains {
		for _, idx := range chain.txs {
			if _, ok := included[idx]; ok {
				continue
			}
			included[idx] = struct{}{}
			proposal = append(proposal, idx)
		}
	}
	// Transactions with no tracked point-key accesses do not appear in any
	// dependency chain. Algorithm 1 still needs them in the proposal, so retain
	// their original deterministic position after all chain-prioritized txs.
	for _, idx := range indices {
		if _, ok := included[idx]; ok {
			continue
		}
		proposal = append(proposal, idx)
	}
	longest := 0
	if len(chains) > 0 {
		longest = len(chains[0].txs)
	}
	var totalCost uint64
	for idx := range trackers {
		totalCost += vegetaCostAt(costs, idx)
	}
	var weightedLongest uint64
	for _, chain := range chains {
		var chainCost uint64
		for _, idx := range chain.txs {
			chainCost += vegetaCostAt(costs, idx)
		}
		if chainCost > weightedLongest {
			weightedLongest = chainCost
		}
	}
	if workers < 1 {
		workers = 1
	}
	workerCapacity := (totalCost + uint64(workers) - 1) / uint64(workers)
	hotKeyLowerBound := weightedLongest
	if workerCapacity > hotKeyLowerBound {
		hotKeyLowerBound = workerCapacity
	}
	return proposal, longest, len(chains), weightedLongest, totalCost, hotKeyLowerBound
}

func vegetaProposalOrderWithStats(trackers []*accessTracker) ([]int, int, int) {
	proposal, longest, chains, _, _, _ := vegetaProposalOrderWithParallelismStats(trackers, nil, 1)
	return proposal, longest, chains
}

func vegetaProposalOrder(trackers []*accessTracker) []int {
	proposal, _, _ := vegetaProposalOrderWithStats(trackers)
	return proposal
}

func buildDependencyMatrix(order []int, trackers []*accessTracker) ([][]dependencyKinds, uint64) {
	matrix := make([][]dependencyKinds, len(order))
	var discovered uint64
	for laterPos := range order {
		matrix[laterPos] = make([]dependencyKinds, len(order))
		for earlierPos := 0; earlierPos < laterPos; earlierPos++ {
			kinds := vegetaDependencyBetween(trackers[order[earlierPos]], trackers[order[laterPos]])
			matrix[laterPos][earlierPos] = kinds
			if kinds != 0 {
				discovered++
			}
		}
	}
	return matrix, discovered
}

// nextVegetaBatch is the direct deterministic analogue of upstream
// popNextBatch: a transaction is ready when it has no remaining WAW dependency
// and does not simultaneously retain RAW and WAR dependencies.
// vegetaReadyState preserves nextVegetaBatch's readiness rule while avoiding
// a full lower-triangular matrix rescan after every completed batch. The old
// implementation revisited every earlier/later pair once per batch, which made
// post-consensus scheduling O(batch_count*n^2) on blocks containing many small
// ready waves. These counters track only dependencies whose earlier endpoint has
// not completed yet, so advancing a batch touches each matrix edge at most once.
type vegetaReadyState struct {
	matrix [][]dependencyKinds
	deps   []int
	raw    []int
	war    []int
	waw    []int
}

func newVegetaReadyState(matrix [][]dependencyKinds) *vegetaReadyState {
	n := len(matrix)
	s := &vegetaReadyState{
		matrix: matrix,
		deps:   make([]int, n),
		raw:    make([]int, n),
		war:    make([]int, n),
		waw:    make([]int, n),
	}
	for laterPos := range matrix {
		for earlierPos := 0; earlierPos < laterPos; earlierPos++ {
			kinds := matrix[laterPos][earlierPos]
			if kinds == 0 {
				continue
			}
			s.deps[laterPos]++
			if kinds&dependencyRAW != 0 {
				s.raw[laterPos]++
			}
			if kinds&dependencyWAR != 0 {
				s.war[laterPos]++
			}
			if kinds&dependencyWAW != 0 {
				s.waw[laterPos]++
			}
		}
	}
	return s
}

func (s *vegetaReadyState) next(done []bool) []int {
	batch := make([]int, 0)
	for pos := range s.matrix {
		if done[pos] {
			continue
		}
		if s.deps[pos] == 0 || (s.waw[pos] == 0 && (s.raw[pos] == 0 || s.war[pos] == 0)) {
			batch = append(batch, pos)
		}
	}
	return batch
}

func (s *vegetaReadyState) markDone(batch []int, done []bool) int {
	completed := 0
	for _, earlierPos := range batch {
		if done[earlierPos] {
			continue
		}
		done[earlierPos] = true
		completed++
		for laterPos := earlierPos + 1; laterPos < len(s.matrix); laterPos++ {
			if done[laterPos] {
				continue
			}
			kinds := s.matrix[laterPos][earlierPos]
			if kinds == 0 {
				continue
			}
			s.deps[laterPos]--
			if kinds&dependencyRAW != 0 {
				s.raw[laterPos]--
			}
			if kinds&dependencyWAR != 0 {
				s.war[laterPos]--
			}
			if kinds&dependencyWAW != 0 {
				s.waw[laterPos]--
			}
		}
	}
	return completed
}

// nextVegetaBatch remains as a reference implementation for tests/debugging.
// VegetaRunner.Run uses vegetaReadyState so production execution does not pay
// the repeated matrix-rescan cost.
func nextVegetaBatch(matrix [][]dependencyKinds, done []bool) []int {
	batch := make([]int, 0)
	for laterPos := range matrix {
		if done[laterPos] {
			continue
		}
		var hasDep, hasWAW, hasRAW, hasWAR bool
		for earlierPos := 0; earlierPos < laterPos; earlierPos++ {
			if done[earlierPos] {
				continue
			}
			kinds := matrix[laterPos][earlierPos]
			if kinds == 0 {
				continue
			}
			hasDep = true
			hasWAW = hasWAW || kinds&dependencyWAW != 0
			hasRAW = hasRAW || kinds&dependencyRAW != 0
			hasWAR = hasWAR || kinds&dependencyWAR != 0
		}
		if !hasDep || (!hasWAW && (!hasRAW || !hasWAR)) {
			batch = append(batch, laterPos)
		}
	}
	return batch
}

func addOrderEdge(edges map[int]map[int]struct{}, from, to int) {
	if from == to {
		return
	}
	if edges[from] == nil {
		edges[from] = make(map[int]struct{})
	}
	edges[from][to] = struct{}{}
}

func topologicalOrder(nodes []int, edges map[int]map[int]struct{}) ([]int, error) {
	inSet := make(map[int]struct{}, len(nodes))
	indegree := make(map[int]int, len(nodes))
	for _, node := range nodes {
		inSet[node] = struct{}{}
		indegree[node] = 0
	}
	for from, tos := range edges {
		if _, ok := inSet[from]; !ok {
			continue
		}
		for to := range tos {
			if _, ok := inSet[to]; ok {
				indegree[to]++
			}
		}
	}
	remaining := append([]int(nil), nodes...)
	sort.Ints(remaining)
	out := make([]int, 0, len(nodes))
	for len(out) < len(nodes) {
		picked := -1
		for _, node := range remaining {
			if indegree[node] == 0 {
				picked = node
				break
			}
		}
		if picked < 0 {
			return nil, fmt.Errorf("dependency graph is cyclic for nodes=%v", nodes)
		}
		out = append(out, picked)
		indegree[picked] = -1
		for to := range edges[picked] {
			if _, ok := indegree[to]; ok && indegree[to] > 0 {
				indegree[to]--
			}
		}
	}
	return out, nil
}

func serializationOrderForSnapshot(nodes []int, positions map[int]int, trackers []*accessTracker) ([]int, error) {
	edges := make(map[int]map[int]struct{})
	for i := 0; i < len(nodes); i++ {
		for j := i + 1; j < len(nodes); j++ {
			left, right := nodes[i], nodes[j]
			if positions[left] > positions[right] {
				left, right = right, left
			}
			kinds := dependencyBetween(trackers[left], trackers[right])
			if kinds&dependencyRAW != 0 && (kinds&dependencyWAR != 0 || kinds&dependencyWAW != 0) {
				return nil, fmt.Errorf("snapshot batch contains non-serializable mixed dependency earlier=%d later=%d kinds=%d", left, right, kinds)
			}
			if kinds&dependencyRAW != 0 {
				// Later reader must serialize before the earlier writer because both
				// executions observed the same batch-start snapshot.
				addOrderEdge(edges, right, left)
			} else if kinds&(dependencyWAR|dependencyWAW) != 0 {
				addOrderEdge(edges, left, right)
			}
		}
	}
	return topologicalOrder(nodes, edges)
}

type vegetaUniverse struct {
	known  map[accessID]struct{}
	points writeSet
}

func buildVegetaUniverse(trackers []*accessTracker) vegetaUniverse {
	universe := vegetaUniverse{known: make(map[accessID]struct{}), points: newWriteSet(64)}
	for _, tracker := range trackers {
		if tracker == nil {
			continue
		}
		for id := range tracker.reads {
			universe.known[id] = struct{}{}
		}
		for id := range tracker.writes.exact {
			universe.known[id] = struct{}{}
		}
		for id := range tracker.writes.collisions {
			universe.known[id] = struct{}{}
		}
		// Vegeta Algorithm 3 defines all_keys as every point key accessed during
		// speculation, not just speculative writes. Pre-consensus Vegeta already
		// retains raw point-read locations for the lock-free replay snapshot, so
		// reuse those locations here for conservative Wasmd iterator validation.
		universe.points.merge(tracker.readLocations)
		universe.points.merge(tracker.writes)
	}
	return universe
}

func sameStoreRange(left, right storeRange) bool {
	return left.store == right.store && bytes.Equal(left.start, right.start) && bytes.Equal(left.end, right.end)
}

func trackerContainsRange(tracker *accessTracker, target storeRange) bool {
	if tracker == nil {
		return false
	}
	for _, existing := range tracker.ranges {
		if sameStoreRange(existing, target) {
			return true
		}
	}
	return false
}

func trackerReadCoversLocation(tracker *accessTracker, id accessID, loc writeLocation) bool {
	if tracker == nil {
		return false
	}
	// Vegeta pre-consensus enables raw point-read retention specifically for
	// replay snapshot/range validation. Use the raw location rather than the
	// fingerprint-only read set so a theoretical 64-bit hash collision cannot
	// hide a newly covered iterator key.
	if writeSetContainsLocation(&tracker.readLocations, id, loc) {
		return true
	}
	for _, r := range tracker.ranges {
		if r.store == loc.store && keyInRange(loc.key, r.start, r.end) {
			return true
		}
	}
	return false
}

type vegetaPointChange struct {
	deferFinal      bool
	newReadIDs      map[accessID]struct{}
	newWriteIDs     map[accessID]struct{}
	newWriteEntries writeSet
}

type vegetaRangeChange struct {
	deferFinal    bool
	newReadRanges []storeRange
}

// classifyVegetaPointChange implements the three point-key cases in Vegeta
// Algorithm 3 directly:
//  1. a newly accessed key that is in all_keys => final serial re-execution;
//  2. a newly read key outside all_keys => wait for the batch's new writes;
//  3. a newly written key outside all_keys => record it, but do not replay the
//     writer solely because the write set grew.
//
// Accesses that disappear during replay require no action.
func classifyVegetaPointChange(pre, actual *accessTracker, universe vegetaUniverse) vegetaPointChange {
	change := vegetaPointChange{
		newReadIDs:      make(map[accessID]struct{}),
		newWriteIDs:     make(map[accessID]struct{}),
		newWriteEntries: newWriteSet(4),
	}
	if pre == nil || actual == nil {
		change.deferFinal = true
		return change
	}
	for id := range actual.reads {
		if _, ok := pre.reads[id]; ok {
			continue
		}
		if _, known := universe.known[id]; known {
			change.deferFinal = true
			continue
		}
		change.newReadIDs[id] = struct{}{}
	}
	forEachWrite(&actual.writes, func(id accessID, loc writeLocation) {
		if writeSetContainsLocation(&pre.writes, id, loc) {
			return
		}
		if _, known := universe.known[id]; known {
			change.deferFinal = true
			return
		}
		change.newWriteIDs[id] = struct{}{}
		change.newWriteEntries.addLocation(id, loc)
	})
	return change
}

// classifyVegetaRangeChange is the Wasmd-specific conservative extension of
// Algorithm 3. Ethereum's point-key model has no iterator/range read. A newly
// introduced range is therefore treated as Case 1 if it covers any speculative
// point key that the transaction did not already read during speculation.
// Otherwise it behaves like a Case-2 new read and only needs replay if another
// transaction in the same ready batch newly writes inside the range.
func classifyVegetaRangeChange(pre, actual *accessTracker, universe vegetaUniverse) vegetaRangeChange {
	change := vegetaRangeChange{}
	if pre == nil || actual == nil {
		change.deferFinal = true
		return change
	}
	for _, actualRange := range actual.ranges {
		if trackerContainsRange(pre, actualRange) {
			continue
		}
		knownOverlap := false
		forEachWrite(&universe.points, func(id accessID, loc writeLocation) {
			if knownOverlap || loc.store != actualRange.store || !keyInRange(loc.key, actualRange.start, actualRange.end) {
				return
			}
			if !trackerReadCoversLocation(pre, id, loc) {
				knownOverlap = true
			}
		})
		if knownOverlap {
			change.deferFinal = true
			continue
		}
		change.newReadRanges = append(change.newReadRanges, actualRange)
	}
	return change
}

type vegetaBatchValidation struct {
	deferred             map[int]struct{}
	immediate            map[int]struct{}
	acceptedOrder        []int
	alg3ValidationNanos  uint64
	rangeValidationNanos uint64
}

// serializationOrderFromVegetaMatrix commits accepted replay results according
// to the dependency information agreed during speculation. It intentionally
// does not recompute a second all-pairs dependency graph from replay trackers:
// Algorithm 3 handles newly introduced keys explicitly, while existing-key
// ordering comes from the consensus-provided DAG.
func serializationOrderFromVegetaMatrix(nodes []int, positions map[int]int, matrix [][]dependencyKinds) ([]int, error) {
	edges := make(map[int]map[int]struct{})
	for i := 0; i < len(nodes); i++ {
		for j := i + 1; j < len(nodes); j++ {
			left, right := nodes[i], nodes[j]
			leftPos, lok := positions[left]
			rightPos, rok := positions[right]
			if !lok || !rok {
				return nil, fmt.Errorf("vegeta serialization missing proposal position left=%d right=%d", left, right)
			}
			if leftPos > rightPos {
				left, right = right, left
				leftPos, rightPos = rightPos, leftPos
			}
			kinds := matrix[rightPos][leftPos]
			switch {
			case kinds&dependencyWAW != 0:
				return nil, fmt.Errorf("vegeta ready batch unexpectedly contains WAW dependency earlier=%d later=%d", left, right)
			case kinds&dependencyRAW != 0:
				// Both transactions executed on the same batch-start snapshot. A RAW
				// pair is therefore equivalent to serializing the reader before writer.
				addOrderEdge(edges, right, left)
			case kinds&dependencyWAR != 0:
				addOrderEdge(edges, left, right)
			}
		}
	}
	return topologicalOrder(nodes, edges)
}

func vegetaValidateBatch(
	batch []int,
	positions map[int]int,
	matrix [][]dependencyKinds,
	preTrackers []*accessTracker,
	actualTrackers []*accessTracker,
	universe vegetaUniverse,
) (vegetaBatchValidation, error) {
	out := vegetaBatchValidation{
		deferred:  make(map[int]struct{}),
		immediate: make(map[int]struct{}),
	}
	pointChanges := make(map[int]vegetaPointChange, len(batch))
	rangeChanges := make(map[int]vegetaRangeChange, len(batch))
	candidates := make(map[int]struct{}, len(batch))

	alg3Started := time.Now()
	for _, idx := range batch {
		change := classifyVegetaPointChange(preTrackers[idx], actualTrackers[idx], universe)
		pointChanges[idx] = change
		if change.deferFinal {
			out.deferred[idx] = struct{}{}
		} else {
			candidates[idx] = struct{}{}
		}
	}
	out.alg3ValidationNanos += uint64(time.Since(alg3Started).Nanoseconds())

	// Wasmd iterator/range reads have no direct equivalent in Vegeta's EVM
	// point-key model. Keep them conservative, but isolate their cost from the
	// paper-faithful point-key Algorithm-3 validation. Transactions deferred by
	// this extension are removed before Algorithm-3 new_keys is constructed,
	// because their first-pass writes will not be committed in this ready batch.
	rangeStarted := time.Now()
	for idx := range candidates {
		change := classifyVegetaRangeChange(preTrackers[idx], actualTrackers[idx], universe)
		rangeChanges[idx] = change
		if change.deferFinal {
			delete(candidates, idx)
			out.deferred[idx] = struct{}{}
		}
	}
	out.rangeValidationNanos += uint64(time.Since(rangeStarted).Nanoseconds())

	// Algorithm 3 Case 2: a read of a key outside all_keys waits until the
	// current ready batch has finished. It is replayed only when another
	// non-deferred transaction newly wrote that same key. Case-3 writers are not
	// replayed merely because they introduced a new write key.
	alg3Started = time.Now()
	writers := make(map[accessID]map[int]struct{})
	for idx := range candidates {
		for id := range pointChanges[idx].newWriteIDs {
			if writers[id] == nil {
				writers[id] = make(map[int]struct{})
			}
			writers[id][idx] = struct{}{}
		}
	}
	for idx := range candidates {
		for id := range pointChanges[idx].newReadIDs {
			for writer := range writers[id] {
				if writer != idx {
					out.immediate[idx] = struct{}{}
					break
				}
			}
			if _, replay := out.immediate[idx]; replay {
				break
			}
		}
	}
	out.alg3ValidationNanos += uint64(time.Since(alg3Started).Nanoseconds())

	rangeStarted = time.Now()
	for idx := range candidates {
		if _, already := out.immediate[idx]; already {
			continue
		}
		for _, readRange := range rangeChanges[idx].newReadRanges {
			unsafe := false
			for writer := range candidates {
				if writer == idx {
					continue
				}
				writerChange := pointChanges[writer]
				forEachWrite(&writerChange.newWriteEntries, func(_ accessID, loc writeLocation) {
					if !unsafe && loc.store == readRange.store && keyInRange(loc.key, readRange.start, readRange.end) {
						unsafe = true
					}
				})
				if unsafe {
					break
				}
			}
			if unsafe {
				out.immediate[idx] = struct{}{}
				break
			}
		}
	}
	out.rangeValidationNanos += uint64(time.Since(rangeStarted).Nanoseconds())

	alg3Started = time.Now()
	accepted := make([]int, 0, len(candidates))
	for _, idx := range batch {
		if _, ok := candidates[idx]; !ok {
			continue
		}
		if _, replay := out.immediate[idx]; replay {
			continue
		}
		accepted = append(accepted, idx)
	}
	order, err := serializationOrderFromVegetaMatrix(accepted, positions, matrix)
	out.alg3ValidationNanos += uint64(time.Since(alg3Started).Nanoseconds())
	if err != nil {
		return vegetaBatchValidation{}, err
	}
	out.acceptedOrder = order
	return out, nil
}

// ariaRule2ForwardFallbacks implements Aria's Rule 2 exactly at the access-set
// level used by the attached Vegeta repository: a later transaction aborts on
// WAW, or when it has both RAW and WAR dependencies against earlier TIDs.
func ariaRule2ForwardFallbacks(trackers []*accessTracker) (map[int]struct{}, uint64) {
	fallbacks := make(map[int]struct{})
	var discovered uint64
	for right := 0; right < len(trackers); right++ {
		if trackers[right] == nil {
			continue
		}
		var waw, war, raw bool
		for left := 0; left < right; left++ {
			if trackers[left] == nil {
				continue
			}
			kinds := dependencyBetween(trackers[left], trackers[right])
			if kinds != 0 {
				discovered++
			}
			waw = waw || kinds&dependencyWAW != 0
			raw = raw || kinds&dependencyRAW != 0
			war = war || kinds&dependencyWAR != 0
		}
		if waw || (war && raw) {
			fallbacks[right] = struct{}{}
		}
	}
	return fallbacks, discovered
}

func ariaAcceptedSerializationOrder(trackers []*accessTracker, fallbacks map[int]struct{}) ([]int, error) {
	nodes := make([]int, 0, len(trackers)-len(fallbacks))
	positions := make(map[int]int, len(trackers))
	for idx := range trackers {
		positions[idx] = idx
		if _, fallback := fallbacks[idx]; !fallback {
			nodes = append(nodes, idx)
		}
	}
	return serializationOrderForSnapshot(nodes, positions, trackers)
}

func ariaDirectPredecessors(fallback []int, trackers []*accessTracker) map[int]map[int]struct{} {
	preds := make(map[int]map[int]struct{}, len(fallback))
	// reachByPos mirrors upstream BuildDAG/buildReach. For each transaction i,
	// scan earlier fallback TIDs backwards. Once an earlier vertex is already
	// reachable through a direct predecessor, do not add a transitive edge.
	reachByPos := make([]map[int]struct{}, len(fallback))
	for laterPos, later := range fallback {
		reach := map[int]struct{}{laterPos: {}}
		direct := make(map[int]struct{})
		for earlierPos := laterPos - 1; earlierPos >= 0; earlierPos-- {
			if _, alreadyReachable := reach[earlierPos]; alreadyReachable {
				continue
			}
			earlier := fallback[earlierPos]
			if dependencyBetween(trackers[earlier], trackers[later]) == 0 {
				continue
			}
			direct[earlier] = struct{}{}
			reach[earlierPos] = struct{}{}
			for reachable := range reachByPos[earlierPos] {
				reach[reachable] = struct{}{}
			}
		}
		reachByPos[laterPos] = reach
		preds[later] = direct
	}
	return preds
}

func ariaFallbackEdges(fallback []int, trackers []*accessTracker) map[int]map[int]struct{} {
	edges := make(map[int]map[int]struct{})
	if len(fallback) == 0 {
		return edges
	}
	directPreds := ariaDirectPredecessors(fallback, trackers)
	chain := hottestAccessChain(fallback, trackers)
	inChain := make(map[int]struct{}, len(chain))
	for _, idx := range chain {
		inChain[idx] = struct{}{}
	}

	// BuildDAG stores direct earlier predecessors for each later transaction.
	// replayAriaP reverses only the direct edges entering a hot-chain node, then
	// removes chain nodes from the ordinary DAG and executes the chain serially.
	for later, predecessors := range directPreds {
		_, laterChain := inChain[later]
		for earlier := range predecessors {
			_, earlierChain := inChain[earlier]
			switch {
			case laterChain && !earlierChain:
				addOrderEdge(edges, later, earlier)
			case laterChain && earlierChain:
				// The serial hot-chain order below replaces internal DAG edges.
			default:
				addOrderEdge(edges, earlier, later)
			}
		}
	}
	for i := 1; i < len(chain); i++ {
		addOrderEdge(edges, chain[i-1], chain[i])
	}

	// Upstream applies the hot-chain reversal after BuildDAG has already
	// transitively reduced the historical conflict graph. Reversing one of those
	// direct edges can therefore destroy the only path that ordered a different
	// (transitively omitted) conflicting pair. The Ethereum implementation does
	// not validate the resulting partial order; on Wasmd that can make two real
	// conflicts execute as if independent and produce a state with no matching
	// serial history.
	//
	// Preserve replayAriaP's intended priority while restoring conflict
	// completeness. Its transformed edges all agree with the deterministic order
	// "hot chain first, then remaining fallback transactions", so adding every
	// missing conflict in that same direction cannot introduce a cycle and does
	// not add serialization between non-conflicting transactions. Transitive
	// edges may be redundant, but they do not reduce exploitable parallelism.
	priority := make([]int, 0, len(fallback))
	priority = append(priority, chain...)
	for _, idx := range fallback {
		if _, hot := inChain[idx]; !hot {
			priority = append(priority, idx)
		}
	}
	for laterPos := 1; laterPos < len(priority); laterPos++ {
		later := priority[laterPos]
		for earlierPos := 0; earlierPos < laterPos; earlierPos++ {
			earlier := priority[earlierPos]
			if dependencyBetween(trackers[earlier], trackers[later]) != 0 {
				addOrderEdge(edges, earlier, later)
			}
		}
	}
	return edges
}

// ariaReleaseSuccessors applies replayAriaP's shrinkDag(done)+popNextTxBatch
// transition to the forward-edge representation used by this port. A successor
// is returned the instant its last predecessor completes; callers do not wait
// for unrelated transactions from the same previous ready set.
func ariaReleaseSuccessors(done int, indegree map[int]int, edges map[int]map[int]struct{}) []int {
	released := make([]int, 0)
	for to := range edges[done] {
		degree, ok := indegree[to]
		if !ok || degree <= 0 {
			continue
		}
		degree--
		indegree[to] = degree
		if degree == 0 {
			released = append(released, to)
		}
	}
	sort.Ints(released)
	return released
}

type ariaFallbackDiagnostics struct {
	dagBuildNanos         uint64
	branchBuildNanos      uint64
	visibilityNanos       uint64
	mvccPublishNanos      uint64
	publishedDeltaEntries uint64
	txExecWorkNanos       uint64
	finalCommitNanos      uint64
}

type ariaFallbackJob struct {
	index  int
	branch *trackingMultiStore
}

// executeAriaFallbackReadyDAG mirrors replayAriaP's completion-driven scheduler:
// transactions become runnable immediately when their last direct predecessor
// finishes, rather than waiting for an entire topological level. The hot chain
// is given dispatch priority, preserving replayAriaP's dedicated serial-chain
// continuity while keeping the benchmark's total worker budget comparable to
// the other strategies (the port does not grant AriaFB an uncounted extra core).
//
// The canonical Wasmd parent is immutable during this speculative fallback. A
// newly ready transaction reads completed DAG-ancestor writes through a
// block-local MVCC view. This preserves predecessor visibility without the old
// O(chain^2) behavior of physically copying every transitive ancestor delta into
// every child branch, and without concurrently mutating a shared CacheMultiStore.
// If a transaction's concrete access footprint differs from the initial Aria
// batch, the speculative fallback is discarded and the caller serially replays
// the subset as a conservative Wasmd-only safety extension.
func executeAriaFallbackReadyDAG(
	ctx context.Context,
	workers int,
	ms storetypes.MultiStore,
	txs [][]byte,
	nodes []int,
	edges map[int]map[int]struct{},
	hotChain []int,
	expected []*accessTracker,
	deliverTx sdk.DeliverTxFunc,
) (map[int]speculativeResult, []int, uint64, uint64, ariaFallbackDiagnostics, bool, error) {
	order, err := topologicalOrder(nodes, edges)
	if err != nil {
		return nil, nil, 0, 0, ariaFallbackDiagnostics{}, false, err
	}
	if len(nodes) == 0 {
		return map[int]speculativeResult{}, order, 0, 0, ariaFallbackDiagnostics{}, false, nil
	}
	if workers < 1 {
		workers = 1
	}
	if workers > len(nodes) {
		workers = len(nodes)
	}

	// Fallback branches also share the staged block parent. Initialize every
	// cache wrapper before workers start so dispatch can safely create private
	// children while predecessor transactions are executing.
	prewarmSpeculativeParent(ms)

	inSet := make(map[int]struct{}, len(nodes))
	indegree := make(map[int]int, len(nodes))
	preds := make(map[int][]int, len(nodes))
	for _, node := range nodes {
		inSet[node] = struct{}{}
		indegree[node] = 0
	}
	for from, tos := range edges {
		if _, ok := inSet[from]; !ok {
			continue
		}
		for to := range tos {
			if _, ok := inSet[to]; !ok {
				continue
			}
			indegree[to]++
			preds[to] = append(preds[to], from)
		}
	}
	orderPos := make(map[int]int, len(order))
	for pos, node := range order {
		orderPos[node] = pos
	}
	visibilityByNode := make(map[int]rustVisibilityMask, len(nodes))
	ancestorSets := make(map[int]map[int]struct{}, len(nodes))
	for _, node := range order {
		set := make(map[int]struct{})
		for _, pred := range preds[node] {
			set[pred] = struct{}{}
			for ancestor := range ancestorSets[pred] {
				set[ancestor] = struct{}{}
			}
		}
		ancestorSets[node] = set
		words := make([]uint64, (len(order)+63)/64)
		for ancestor := range set {
			markRustCompleted(words, orderPos[ancestor])
		}
		visibilityByNode[node] = rustVisibilityMask{words: words}
	}
	versions := newRustBlockMVCC()

	hot := make(map[int]struct{}, len(hotChain))
	for _, idx := range hotChain {
		hot[idx] = struct{}{}
	}
	lessReady := func(a, b int) bool {
		_, ah := hot[a]
		_, bh := hot[b]
		if ah != bh {
			return ah
		}
		return a < b
	}
	ready := make([]int, 0, len(nodes))
	for _, node := range nodes {
		if indegree[node] == 0 {
			ready = append(ready, node)
		}
	}
	sort.Slice(ready, func(i, j int) bool { return lessReady(ready[i], ready[j]) })

	jobs := make(chan ariaFallbackJob, workers)
	done := make(chan speculativeResult, len(nodes))
	var wg sync.WaitGroup
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for job := range jobs {
				select {
				case <-ctx.Done():
					done <- speculativeResult{index: job.index, store: job.branch, result: &abci.ExecTxResult{Code: 1, Log: ctx.Err().Error()}}
					continue
				default:
				}
				execStarted := time.Now()
				res := deliverTx(txs[job.index], nil, job.branch, job.index, map[string]any{})
				execEnded := time.Now()
				done <- speculativeResult{index: job.index, store: job.branch, result: res, attempt: 1, execNanos: uint64(execEnded.Sub(execStarted).Nanoseconds()), execStartNanos: execStarted.UnixNano(), execEndNanos: execEnded.UnixNano()}
			}
		}()
	}

	results := make(map[int]speculativeResult, len(nodes))
	deltas := make(map[int]rustTxDelta, len(nodes))
	active := 0
	finished := 0
	var attempts uint64
	var validationNanos uint64
	var diag ariaFallbackDiagnostics
	unstable := false

	drainAndClose := func() {
		for active > 0 {
			<-done
			active--
			attempts++
		}
		close(jobs)
		wg.Wait()
	}
	dispatch := func() error {
		for !unstable && active < workers && len(ready) > 0 {
			idx := ready[0]
			ready = ready[1:]
			visibilityStarted := time.Now()
			visibility := visibilityByNode[idx]
			diag.visibilityNanos += uint64(time.Since(visibilityStarted).Nanoseconds())
			branchStarted := time.Now()
			branch := newSpeculativeTrackingMultiStoreWithMVCC(ms, &rustMvccReadView{
				versions:       versions,
				canonicalIndex: orderPos[idx],
				visibility:     visibility,
			})
			diag.branchBuildNanos += uint64(time.Since(branchStarted).Nanoseconds())
			jobs <- ariaFallbackJob{index: idx, branch: branch}
			active++
		}
		return nil
	}

	for finished < len(nodes) {
		if err := dispatch(); err != nil {
			drainAndClose()
			return nil, nil, attempts, validationNanos, diag, false, err
		}
		if active == 0 {
			close(jobs)
			wg.Wait()
			return nil, nil, attempts, validationNanos, diag, false, fmt.Errorf("aria-fb fallback DAG stalled finished=%d total=%d", finished, len(nodes))
		}
		result := <-done
		active--
		attempts++
		diag.txExecWorkNanos += result.execNanos
		if result.result != nil && result.result.Code != 0 {
			drainAndClose()
			return nil, nil, attempts, validationNanos, diag, false, &ariaSemanticExecutionError{phase: "fallback", index: result.index, log: result.result.Log}
		}
		if result.store == nil || result.index < 0 || result.index >= len(expected) {
			drainAndClose()
			return nil, nil, attempts, validationNanos, diag, false, fmt.Errorf("aria-fb invalid fallback result tx=%d", result.index)
		}
		delta, deltaErr := captureRustDelta(result.store)
		if deltaErr != nil {
			drainAndClose()
			return nil, nil, attempts, validationNanos, diag, false, fmt.Errorf("aria-fb capture fallback tx %d delta: %w", result.index, deltaErr)
		}
		publishStarted := time.Now()
		versions.publish(orderPos[result.index], delta)
		diag.mvccPublishNanos += uint64(time.Since(publishStarted).Nanoseconds())
		diag.publishedDeltaEntries += uint64(len(delta.entries))
		validationStarted := time.Now()
		if !accessTrackersEqual(expected[result.index], result.store.tracker) {
			unstable = true
		}
		validationNanos += uint64(time.Since(validationStarted).Nanoseconds())
		if unstable {
			drainAndClose()
			return nil, order, attempts, validationNanos, diag, true, nil
		}

		results[result.index] = result
		deltas[result.index] = delta
		finished++
		indegree[result.index] = -1
		ready = append(ready, ariaReleaseSuccessors(result.index, indegree, edges)...)
		sort.Slice(ready, func(i, j int) bool { return lessReady(ready[i], ready[j]) })
	}
	close(jobs)
	wg.Wait()
	commitStarted := time.Now()
	for _, idx := range order {
		if err := applyRustDelta(ms, deltas[idx]); err != nil {
			return nil, nil, attempts, validationNanos, diag, false, fmt.Errorf("aria-fb commit fallback tx %d delta: %w", idx, err)
		}
	}
	diag.finalCommitNanos = uint64(time.Since(commitStarted).Nanoseconds())
	return results, order, attempts, validationNanos, diag, false, nil
}

// AriaFBRunner adapts the attached repository's AriaFB mechanism to Wasmd:
// execute one Aria batch on the block-start snapshot, accept Rule-2 survivors in
// a serialization order consistent with that snapshot, and send Rule-2 aborts
// through a hot-chain-prioritized dependency-DAG fallback replay. The fallback
// keeps a conservative dynamic-access safety replay for Cosmos iterator/key-set
// changes, which Ethereum's address-level implementation does not need.
type AriaFBRunner struct {
	workers            int
	last               policyRunStats
	serializationOrder []int
}

func NewAriaFBRunner(workers int) *AriaFBRunner   { return &AriaFBRunner{workers: workers} }
func (r *AriaFBRunner) LastStats() policyRunStats { return r.last }
func (r *AriaFBRunner) LastSerializationOrder() []int {
	return append([]int(nil), r.serializationOrder...)
}

func (r *AriaFBRunner) MarkCanonicalFallback(txCount int) {
	r.serializationOrder = r.serializationOrder[:0]
	for i := 0; i < txCount; i++ {
		r.serializationOrder = append(r.serializationOrder, i)
	}
}

func (r *AriaFBRunner) Run(ctx context.Context, ms storetypes.MultiStore, txs [][]byte, deliverTx sdk.DeliverTxFunc) ([]*abci.ExecTxResult, error) {
	indices := make([]int, len(txs))
	for i := range indices {
		indices[i] = i
	}
	results := make([]*abci.ExecTxResult, len(txs))
	r.serializationOrder = r.serializationOrder[:0]
	if len(indices) == 0 {
		r.last = policyRunStats{}
		return results, nil
	}

	stats := policyRunStats{Speculated: uint64(len(indices))}
	postStarted := time.Now()
	initialStarted := time.Now()
	initial := speculateIndices(ctx, r.workers, ms, txs, indices, deliverTx)
	stats.AriaInitialBatchNanos = uint64(time.Since(initialStarted).Nanoseconds())
	trackers := make([]*accessTracker, len(txs))
	for idx, result := range initial {
		if result.store == nil {
			return nil, fmt.Errorf("aria-fb missing initial execution tx=%d", idx)
		}
		if result.result != nil && result.result.Code != 0 {
			stats.Attempts = uint64(len(indices))
			stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
			r.last = stats
			return nil, &ariaSemanticExecutionError{phase: "initial", index: idx, log: result.result.Log}
		}
		trackers[idx] = result.store.tracker
		stats.AriaInitialExecWorkNanos += result.execNanos
	}

	analysisStarted := time.Now()
	fallbacks, discovered := ariaRule2ForwardFallbacks(trackers)
	acceptedOrder, err := ariaAcceptedSerializationOrder(trackers, fallbacks)
	if err != nil {
		return nil, err
	}
	stats.ConflictAnalysisNanos = uint64(time.Since(analysisStarted).Nanoseconds())
	stats.DiscoveredConflicts = discovered
	stats.ForwardFallbacks = uint64(len(fallbacks))
	stats.Attempts = uint64(len(indices))
	stats.Reused = uint64(len(indices) - len(fallbacks))
	stats.Replayed = uint64(len(fallbacks))

	acceptedCommitStarted := time.Now()
	for _, idx := range acceptedOrder {
		initial[idx].store.Write()
		results[idx] = initial[idx].result
		r.serializationOrder = append(r.serializationOrder, idx)
	}
	stats.AriaAcceptedCommitNanos = uint64(time.Since(acceptedCommitStarted).Nanoseconds())

	fallback := make([]int, 0, len(fallbacks))
	for idx := range fallbacks {
		fallback = append(fallback, idx)
	}
	sort.Ints(fallback)
	if len(fallback) > 0 {
		replayStarted := time.Now()
		dagStarted := time.Now()
		edges := ariaFallbackEdges(fallback, trackers)
		hotChain := hottestAccessChain(fallback, trackers)
		stats.AriaFallbackDAGBuildNanos = uint64(time.Since(dagStarted).Nanoseconds())
		fallbackSpec, fallbackOrder, attempts, validationNanos, fallbackDiag, unstable, err := executeAriaFallbackReadyDAG(
			ctx, r.workers, ms, txs, fallback, edges, hotChain, trackers, deliverTx,
		)
		stats.Attempts += attempts
		stats.ValidationNanos += validationNanos
		stats.AriaFallbackBranchBuildNanos += fallbackDiag.branchBuildNanos
		stats.AriaFallbackVisibilityNanos += fallbackDiag.visibilityNanos
		stats.AriaFallbackMVCCPublishNanos += fallbackDiag.mvccPublishNanos
		stats.AriaFallbackPublishedDeltaEntries += fallbackDiag.publishedDeltaEntries
		stats.AriaFallbackTxExecWorkNanos += fallbackDiag.txExecWorkNanos
		stats.AriaFallbackFinalCommitNanos += fallbackDiag.finalCommitNanos
		if err != nil {
			stats.ReplayExecutionNanos = uint64(time.Since(replayStarted).Nanoseconds())
			stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
			stats.Reexecutions = stats.Attempts - uint64(len(txs))
			r.last = stats
			return nil, err
		}
		if unstable {
			// The released Ethereum implementation assumes replay accesses match
			// the initial Aria batch. Wasmd translations can change key/range sets;
			// discard the staged fallback cache and deterministically serialize the
			// whole fallback subset rather than letting that adaptation weaken safety.
			for _, idx := range fallbackOrder {
				final := replayOne(ms, txs[idx], idx, deliverTx)
				stats.Attempts++
				stats.SafetyReplays++
				if final.result != nil && final.result.Code != 0 {
					stats.ReplayExecutionNanos = uint64(time.Since(replayStarted).Nanoseconds())
					stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
					stats.Reexecutions = stats.Attempts - uint64(len(txs))
					r.last = stats
					return nil, &ariaSemanticExecutionError{phase: "safety replay", index: idx, log: final.result.Log}
				}
				final.store.Write()
				results[idx] = final.result
			}
		} else {
			for _, idx := range fallbackOrder {
				result, ok := fallbackSpec[idx]
				if !ok || result.result == nil {
					return nil, fmt.Errorf("aria-fb missing committed fallback result tx=%d", idx)
				}
				results[idx] = result.result
			}
		}
		r.serializationOrder = append(r.serializationOrder, fallbackOrder...)
		stats.ReplayExecutionNanos = uint64(time.Since(replayStarted).Nanoseconds())
	}
	stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
	stats.Reexecutions = stats.Attempts - uint64(len(txs))
	r.last = stats
	return results, ctx.Err()
}

// VegetaRunner ports the attached repository's SpeculateMod + ParallelMod
// semantics to the same Wasmd/Cosmos substrate. Pre-consensus execution is used
// only to discover actual accesses, sort all point-key dependency chains from
// longest to shortest (Algorithm 1), and build the dependency matrix. After consensus every transaction is replayed in
// Rule-2-compatible DAG batches; only access-set changes that cannot be safely
// committed under Vegeta's new-key rules are executed again at the end.
type VegetaRunner struct {
	workers            int
	last               policyRunStats
	proposalOrder      []int
	serializationOrder []int
	estimatedCosts     []uint32
}

func NewVegetaRunner(workers int) *VegetaRunner   { return &VegetaRunner{workers: workers} }
func (r *VegetaRunner) LastStats() policyRunStats { return r.last }
func (r *VegetaRunner) SetEstimatedCosts(costs []uint32) {
	r.estimatedCosts = append(r.estimatedCosts[:0], costs...)
}
func (r *VegetaRunner) LastProposalOrder() []int {
	return append([]int(nil), r.proposalOrder...)
}
func (r *VegetaRunner) LastSerializationOrder() []int {
	return append([]int(nil), r.serializationOrder...)
}

func (r *VegetaRunner) MarkCanonicalFallback(txCount int) {
	r.serializationOrder = r.serializationOrder[:0]
	for i := 0; i < txCount; i++ {
		r.serializationOrder = append(r.serializationOrder, i)
	}
}

func (r *VegetaRunner) Run(ctx context.Context, ms storetypes.MultiStore, txs [][]byte, deliverTx sdk.DeliverTxFunc) ([]*abci.ExecTxResult, error) {
	indices := make([]int, len(txs))
	for i := range indices {
		indices[i] = i
	}
	results := make([]*abci.ExecTxResult, len(txs))
	r.proposalOrder = r.proposalOrder[:0]
	r.serializationOrder = r.serializationOrder[:0]
	if len(indices) == 0 {
		r.last = policyRunStats{}
		return results, nil
	}

	stats := policyRunStats{Speculated: uint64(len(indices))}
	preStarted := time.Now()
	preSpec := speculateAccesses(ctx, r.workers, ms, txs, indices, deliverTx)
	preTrackers := make([]*accessTracker, len(txs))
	preStores := newStoreIDRegistry()
	var preFirstStart, preLastEnd int64
	for idx, result := range preSpec {
		if result.tracker == nil || result.stores == nil {
			return nil, fmt.Errorf("vegeta missing speculative execution tx=%d", idx)
		}
		if result.result != nil && result.result.Code != 0 {
			stats.PreConsensusNanos = uint64(time.Since(preStarted).Nanoseconds())
			r.last = stats
			return nil, &vegetaSemanticExecutionError{phase: "pre-speculation", index: idx, log: result.result.Log}
		}
		preTrackers[idx] = result.tracker
		preStores.merge(result.stores)
		stats.PreExecWorkNanos += result.execNanos
		if result.execStartNanos > 0 && (preFirstStart == 0 || result.execStartNanos < preFirstStart) {
			preFirstStart = result.execStartNanos
		}
		if result.execEndNanos > preLastEnd {
			preLastEnd = result.execEndNanos
		}
	}
	if preFirstStart > 0 && preLastEnd >= preFirstStart {
		stats.PreExecSpanNanos = uint64(preLastEnd - preFirstStart)
	}
	analysisStarted := time.Now()
	var longestChain, chainCount int
	proposalOrder, longestChain, chainCount, weightedLongest, totalEstimatedCost, hotKeyLowerBound := vegetaProposalOrderWithParallelismStats(preTrackers, r.estimatedCosts, r.workers)
	r.proposalOrder = proposalOrder
	stats.VegetaLongestChain = uint64(longestChain)
	stats.VegetaChainCount = uint64(chainCount)
	stats.VegetaWeightedLongestChainCost = weightedLongest
	stats.VegetaTotalEstimatedCost = totalEstimatedCost
	stats.VegetaHotKeyWorkerLowerBoundCost = hotKeyLowerBound
	matrix, discovered := buildDependencyMatrix(r.proposalOrder, preTrackers)
	universe := buildVegetaUniverse(preTrackers)
	stats.ConflictAnalysisNanos = uint64(time.Since(analysisStarted).Nanoseconds())
	stats.DiscoveredConflicts = discovered
	stats.PreConsensusNanos = uint64(time.Since(preStarted).Nanoseconds())

	positions := make(map[int]int, len(r.proposalOrder))
	for pos, idx := range r.proposalOrder {
		positions[idx] = pos
	}
	done := make([]bool, len(r.proposalOrder))
	ready := newVegetaReadyState(matrix)
	deferred := make(map[int]struct{})
	postStarted := time.Now()
	for completed := 0; completed < len(r.proposalOrder); {
		readyStarted := time.Now()
		batchPositions := ready.next(done)
		stats.ReadySelectionNanos += uint64(time.Since(readyStarted).Nanoseconds())
		if len(batchPositions) == 0 {
			return nil, fmt.Errorf("vegeta replay DAG stalled completed=%d total=%d", completed, len(r.proposalOrder))
		}
		batch := make([]int, 0, len(batchPositions))
		var batchCost uint64
		var maxBatchCost uint64
		for _, pos := range batchPositions {
			idx := r.proposalOrder[pos]
			batch = append(batch, idx)
			cost := vegetaCostAt(r.estimatedCosts, idx)
			batchCost += cost
			if cost > maxBatchCost {
				maxBatchCost = cost
			}
		}
		workerCapacity := (batchCost + uint64(max(1, r.workers)) - 1) / uint64(max(1, r.workers))
		if maxBatchCost > workerCapacity {
			workerCapacity = maxBatchCost
		}
		stats.VegetaReadyWorkerLowerBoundCost += workerCapacity
		snapshotStarted := time.Now()
		readSnapshot, err := buildVegetaReadSnapshot(ms, batch, preTrackers, preStores)
		if err != nil {
			return nil, fmt.Errorf("vegeta build post-consensus read snapshot: %w", err)
		}
		stats.SnapshotBuildNanos += uint64(time.Since(snapshotStarted).Nanoseconds())
		stats.PostBatches++
		if len(batch) == 1 {
			stats.PostSingletonBatches++
		}
		if uint64(len(batch)) > stats.PostMaxBatch {
			stats.PostMaxBatch = uint64(len(batch))
		}
		postSpec := speculateIndicesWithSnapshot(ctx, r.workers, ms, txs, batch, deliverTx, readSnapshot, false)
		var batchFirstStart, batchLastEnd int64
		snapshotStats := readSnapshot.stats()
		stats.SnapshotPointHits += snapshotStats.PointHits
		stats.SnapshotPointMisses += snapshotStats.PointMisses
		stats.SnapshotRangeHits += snapshotStats.RangeHits
		stats.SnapshotRangeMisses += snapshotStats.RangeMisses
		stats.Attempts += uint64(len(batch))
		actualTrackers := make([]*accessTracker, len(txs))
		for _, idx := range batch {
			result, ok := postSpec[idx]
			if !ok || result.store == nil {
				return nil, fmt.Errorf("vegeta missing replay execution tx=%d", idx)
			}
			if result.result != nil && result.result.Code != 0 {
				stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
				stats.Reexecutions = uint64(len(deferred))
				r.last = stats
				return nil, &vegetaSemanticExecutionError{phase: "post-consensus replay", index: idx, log: result.result.Log}
			}
			actualTrackers[idx] = result.store.tracker
			stats.PostExecWorkNanos += result.execNanos
			if len(batch) > 1 {
				stats.PostWideExecWorkNanos += result.execNanos
			}
			if result.execStartNanos > 0 && (batchFirstStart == 0 || result.execStartNanos < batchFirstStart) {
				batchFirstStart = result.execStartNanos
			}
			if result.execEndNanos > batchLastEnd {
				batchLastEnd = result.execEndNanos
			}
		}
		if batchFirstStart > 0 && batchLastEnd >= batchFirstStart {
			span := uint64(batchLastEnd - batchFirstStart)
			stats.PostExecSpanNanos += span
			if len(batch) > 1 {
				stats.PostWideExecSpanNanos += span
				stats.PostWideTransactions += uint64(len(batch))
			}
		}
		validationStarted := time.Now()
		validation, err := vegetaValidateBatch(batch, positions, matrix, preTrackers, actualTrackers, universe)
		stats.ValidationNanos += uint64(time.Since(validationStarted).Nanoseconds())
		stats.VegetaAlg3ValidationNanos += validation.alg3ValidationNanos
		stats.VegetaRangeValidationNanos += validation.rangeValidationNanos
		if err != nil {
			return nil, err
		}
		for idx := range validation.deferred {
			deferred[idx] = struct{}{}
		}
		for _, idx := range validation.acceptedOrder {
			postSpec[idx].store.Write()
			results[idx] = postSpec[idx].result
			stats.Reused++
			r.serializationOrder = append(r.serializationOrder, idx)
		}
		if len(validation.immediate) > 0 {
			replayStarted := time.Now()
			for _, idx := range r.proposalOrder {
				if _, ok := validation.immediate[idx]; !ok {
					continue
				}
				final := replayOne(ms, txs[idx], idx, deliverTx)
				stats.Attempts++
				stats.SafetyReplays++
				if final.result != nil && final.result.Code != 0 {
					stats.ReplayExecutionNanos += uint64(time.Since(replayStarted).Nanoseconds())
					stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
					stats.Reexecutions = uint64(len(deferred))
					r.last = stats
					return nil, &vegetaSemanticExecutionError{phase: "new-key safety replay", index: idx, log: final.result.Log}
				}
				final.store.Write()
				results[idx] = final.result
				r.serializationOrder = append(r.serializationOrder, idx)
			}
			replayNanos := uint64(time.Since(replayStarted).Nanoseconds())
			stats.ReplayExecutionNanos += replayNanos
			stats.VegetaIntrinsicReexecutionNanos += replayNanos
		}
		readyStarted = time.Now()
		completed += ready.markDone(batchPositions, done)
		stats.ReadySelectionNanos += uint64(time.Since(readyStarted).Nanoseconds())
	}

	if len(deferred) > 0 {
		replayStarted := time.Now()
		for _, idx := range r.proposalOrder {
			if _, ok := deferred[idx]; !ok {
				continue
			}
			final := replayOne(ms, txs[idx], idx, deliverTx)
			stats.Attempts++
			stats.Replayed++
			if final.result != nil && final.result.Code != 0 {
				stats.ReplayExecutionNanos += uint64(time.Since(replayStarted).Nanoseconds())
				stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
				stats.Reexecutions = uint64(len(deferred))
				r.last = stats
				return nil, &vegetaSemanticExecutionError{phase: "final re-execution", index: idx, log: final.result.Log}
			}
			final.store.Write()
			results[idx] = final.result
			r.serializationOrder = append(r.serializationOrder, idx)
		}
		replayNanos := uint64(time.Since(replayStarted).Nanoseconds())
		stats.ReplayExecutionNanos += replayNanos
		stats.VegetaIntrinsicReexecutionNanos += replayNanos
	}
	stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
	// Vegeta's paper re-execution rate includes both TxsRe (known-key access
	// changes) and Case-2 readers replayed immediately after their ready batch.
	// SafetyReplays remains the immediate-reader subset for diagnostics.
	stats.Reexecutions = uint64(len(deferred)) + stats.SafetyReplays
	r.last = stats
	return results, ctx.Err()
}

type staticFootprint struct {
	accesses []predictedAccess
}

func newStaticFootprint() staticFootprint {
	return staticFootprint{}
}

func staticDepends(earlier, later staticFootprint) bool {
	// Match the native Rust scheduler exactly: predicted_conflict is symmetric
	// and creates an earlier->later edge whenever an overlapping symbolic
	// location has at least one write. This deliberately includes read/write,
	// write/read, and write/write pairs.
	return predictedConflict(earlier, later)
}

func buildStaticLevels(block ExecutionBlock, accesses symbolicAccessIndex) ([][]int, error) {
	n := len(block.Transactions)
	if n == 0 {
		return nil, nil
	}
	fps := make([]staticFootprint, n)
	levels := make([]int, n)
	maxLevel := 0
	for i := range block.Transactions {
		fp, ok := accesses.footprint(block.BlockNumber, i)
		if !ok {
			return nil, fmt.Errorf("missing symbolic access prediction block=%d tx=%d", block.BlockNumber, i)
		}
		fps[i] = fp
		for j := 0; j < i; j++ {
			if staticDepends(fps[j], fps[i]) && levels[i] <= levels[j] {
				levels[i] = levels[j] + 1
			}
		}
		if levels[i] > maxLevel {
			maxLevel = levels[i]
		}
	}
	out := make([][]int, maxLevel+1)
	for idx, level := range levels {
		out[level] = append(out[level], idx)
	}
	return out, nil
}

// SymbGraphStaticRunner ports the deployable static policy to the same Wasmd
// execution substrate. The static graph controls speculative waves; actual SDK
// read/write validation remains authoritative and replays prediction misses.
type SymbGraphStaticRunner struct {
	workers  int
	block    ExecutionBlock
	accesses symbolicAccessIndex
	last     policyRunStats
}

func NewSymbGraphStaticRunner(workers int, block ExecutionBlock, accesses symbolicAccessIndex) *SymbGraphStaticRunner {
	return &SymbGraphStaticRunner{workers: workers, block: block, accesses: accesses}
}
func (r *SymbGraphStaticRunner) LastStats() policyRunStats { return r.last }

func (r *SymbGraphStaticRunner) Run(ctx context.Context, ms storetypes.MultiStore, txs [][]byte, deliverTx sdk.DeliverTxFunc) ([]*abci.ExecTxResult, error) {
	if len(txs) != len(r.block.Transactions) {
		return nil, fmt.Errorf("static runner block/tx mismatch: plan=%d runner=%d", len(r.block.Transactions), len(txs))
	}
	results := make([]*abci.ExecTxResult, len(txs))
	levels, err := buildStaticLevels(r.block, r.accesses)
	if err != nil {
		return nil, err
	}
	var attempts atomic.Uint64
	stats := policyRunStats{}
	for _, indices := range levels {
		if len(indices) == 0 {
			continue
		}
		stats.Speculated += uint64(len(indices))
		// Every level starts after the prior level has committed. Within a level,
		// all transactions share the same snapshot and actual access validation
		// handles static prediction misses.
		spec := speculateIndices(ctx, r.workers, ms, txs, indices, deliverTx)
		levelWrites := newWriteSet(64)
		if err := commitSpeculation(ms, txs, indices, spec, deliverTx, results, &levelWrites, &attempts, &stats); err != nil {
			return nil, err
		}
	}
	a := attempts.Load()
	stats.Attempts = a
	stats.Reexecutions = a - uint64(len(txs))
	r.last = stats
	return results, ctx.Err()
}
