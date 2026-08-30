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
	Attempts              uint64
	Reexecutions          uint64
	Speculated            uint64
	Reused                uint64
	Replayed              uint64
	PreConsensusNanos     uint64
	PostConsensusNanos    uint64
	ValidationNanos       uint64
	ReplayExecutionNanos  uint64
	ConflictAnalysisNanos uint64
	DiscoveredConflicts   uint64
	ForwardFallbacks      uint64
	SafetyReplays         uint64
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
	reads  map[accessID]struct{}
	ranges []storeRange
	writes writeSet
	parent *accessTracker
}

func newAccessTracker(parent *accessTracker) *accessTracker {
	return &accessTracker{
		reads:  make(map[accessID]struct{}, 64),
		writes: newWriteSet(16),
		parent: parent,
	}
}

func (t *accessTracker) read(store storeID, key []byte) {
	id := exactAccessID(store, key)
	t.reads[id] = struct{}{}
	// A discarded nested CacheContext can still influence control flow. Bubble
	// reads up immediately; writes only bubble when that cache is committed.
	if t.parent != nil {
		t.parent.readID(id)
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

// trackingKVStore records actual Wasmd/Cosmos accesses while delegating storage
// semantics to the SDK cache store itself.
type trackingKVStore struct {
	storetypes.KVStore
	storeKey storetypes.StoreKey
	store    storeID
	tracker  *accessTracker
	readView *rustMvccReadView
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
	var underlying storetypes.Iterator
	if reverse {
		underlying = s.KVStore.ReverseIterator(start, end)
	} else {
		underlying = s.KVStore.Iterator(start, end)
	}
	values, err := collectByteIterator(underlying)
	if err != nil {
		return rustByteErrorIterator(start, end, err)
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
	tracker  *accessTracker
	stores   *storeIDRegistry
	readView *rustMvccReadView
	overlay  *rustLocalOverlay
}

func wrapTrackingCacheMultiStore(store storetypes.CacheMultiStore, parent *accessTracker, stores *storeIDRegistry, readView *rustMvccReadView, parentOverlay *rustLocalOverlay) *trackingMultiStore {
	return &trackingMultiStore{
		cacheMultiStoreDelegate: cacheMultiStoreDelegate{CacheMultiStore: store},
		tracker:                 newAccessTracker(parent),
		stores:                  stores,
		readView:                readView,
		overlay:                 newRustLocalOverlay(parentOverlay),
	}
}

func newTrackingMultiStore(parent storetypes.MultiStore) *trackingMultiStore {
	return wrapTrackingCacheMultiStore(parent.CacheMultiStore(), nil, newStoreIDRegistry(), nil, nil)
}

func newTrackingMultiStoreWithMVCC(parent storetypes.MultiStore, readView *rustMvccReadView) *trackingMultiStore {
	return wrapTrackingCacheMultiStore(parent.CacheMultiStore(), nil, newStoreIDRegistry(), readView, nil)
}

func (m *trackingMultiStore) CacheWrap() storetypes.CacheWrap { return m.CacheMultiStore() }

func (m *trackingMultiStore) CacheMultiStore() storetypes.CacheMultiStore {
	child := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStore()
	return wrapTrackingCacheMultiStore(child, m.tracker, m.stores, m.readView, m.overlay)
}

func (m *trackingMultiStore) CacheMultiStoreWithVersion(version int64) (storetypes.CacheMultiStore, error) {
	child, err := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStoreWithVersion(version)
	if err != nil {
		return nil, err
	}
	return wrapTrackingCacheMultiStore(child, m.tracker, m.stores, m.readView, m.overlay), nil
}

func (m *trackingMultiStore) GetStore(key storetypes.StoreKey) storetypes.Store {
	store := m.cacheMultiStoreDelegate.CacheMultiStore.GetStore(key)
	if kv, ok := store.(storetypes.KVStore); ok {
		return trackingKVStore{KVStore: kv, storeKey: key, store: m.stores.id(key), tracker: m.tracker, readView: m.readView, overlay: m.overlay}
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
	m.overlay.mergeIntoParent()
}

type speculativeResult struct {
	index   int
	store   *trackingMultiStore
	result  *abci.ExecTxResult
	attempt uint64
}

func prepareSpeculation(ms storetypes.MultiStore, indices []int) map[int]*trackingMultiStore {
	out := make(map[int]*trackingMultiStore, len(indices))
	// Branch creation is intentionally serialized. Execution on the independent
	// branches is parallel; this avoids depending on CacheMultiStore branch
	// construction itself being thread-safe.
	for _, idx := range indices {
		out[idx] = newTrackingMultiStore(ms)
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
	if workers < 1 {
		workers = 1
	}
	branches := prepareSpeculation(ms, indices)
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
				res := deliverTx(txs[idx], nil, branch, idx, map[string]any{})
				results <- speculativeResult{index: idx, store: branch, result: res, attempt: 1}
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

func replayOne(ms storetypes.MultiStore, tx []byte, idx int, deliverTx sdk.DeliverTxFunc) speculativeResult {
	branch := newTrackingMultiStore(ms)
	res := deliverTx(tx, nil, branch, idx, map[string]any{})
	return speculativeResult{index: idx, store: branch, result: res, attempt: 1}
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

// vegetaDependencyBetween matches BuildDAGShowDependencies in the attached
// Vegeta repository. That code assigns exactly one dependency class per pair
// with WAW taking precedence over RAW, which takes precedence over WAR.
func vegetaDependencyBetween(earlier, later *accessTracker) dependencyKinds {
	kinds := dependencyBetween(earlier, later)
	switch {
	case kinds&dependencyWAW != 0:
		return dependencyWAW
	case kinds&dependencyRAW != 0:
		return dependencyRAW
	case kinds&dependencyWAR != 0:
		return dependencyWAR
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

func vegetaProposalOrder(trackers []*accessTracker) []int {
	indices := make([]int, len(trackers))
	for i := range indices {
		indices[i] = i
	}
	chain := hottestAccessChain(indices, trackers)
	if len(chain) == 0 {
		return indices
	}
	inChain := make(map[int]struct{}, len(chain))
	proposal := make([]int, 0, len(indices))
	for _, idx := range chain {
		inChain[idx] = struct{}{}
		proposal = append(proposal, idx)
	}
	for _, idx := range indices {
		if _, ok := inChain[idx]; !ok {
			proposal = append(proposal, idx)
		}
	}
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
	writes writeSet
}

func buildVegetaUniverse(trackers []*accessTracker) vegetaUniverse {
	universe := vegetaUniverse{known: make(map[accessID]struct{}), writes: newWriteSet(64)}
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
		universe.writes.merge(tracker.writes)
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
	if _, ok := tracker.reads[id]; ok {
		return true
	}
	for _, r := range tracker.ranges {
		if r.store == loc.store && keyInRange(loc.key, r.start, r.end) {
			return true
		}
	}
	return false
}

func rangeOverlapsWrites(target storeRange, writes *writeSet) bool {
	overlaps := false
	forEachWrite(writes, func(_ accessID, loc writeLocation) {
		if loc.store == target.store && keyInRange(loc.key, target.start, target.end) {
			overlaps = true
		}
	})
	return overlaps
}

type vegetaAccessChange struct {
	changed         bool
	newReadIDs      map[accessID]struct{}
	newReadRanges   []storeRange
	newWriteIDs     map[accessID]struct{}
	newWriteEntries writeSet
}

func classifyVegetaAccessChange(pre, actual *accessTracker, universe vegetaUniverse) vegetaAccessChange {
	change := vegetaAccessChange{
		newReadIDs:      make(map[accessID]struct{}),
		newWriteIDs:     make(map[accessID]struct{}),
		newWriteEntries: newWriteSet(4),
	}
	if pre == nil || actual == nil {
		change.changed = true
		return change
	}
	for id := range actual.reads {
		if _, ok := pre.reads[id]; ok {
			continue
		}
		if _, known := universe.known[id]; known {
			if writeSetHasID(&universe.writes, id) {
				change.changed = true
			}
			continue
		}
		change.newReadIDs[id] = struct{}{}
	}
	for _, actualRange := range actual.ranges {
		if trackerContainsRange(pre, actualRange) {
			continue
		}
		rangeChanged := false
		forEachWrite(&universe.writes, func(id accessID, loc writeLocation) {
			if rangeChanged || loc.store != actualRange.store || !keyInRange(loc.key, actualRange.start, actualRange.end) {
				return
			}
			if !trackerReadCoversLocation(pre, id, loc) {
				rangeChanged = true
			}
		})
		if rangeChanged {
			change.changed = true
		} else {
			change.newReadRanges = append(change.newReadRanges, actualRange)
		}
	}
	forEachWrite(&actual.writes, func(id accessID, loc writeLocation) {
		if writeSetContainsLocation(&pre.writes, id, loc) {
			return
		}
		if _, known := universe.known[id]; known {
			change.changed = true
			return
		}
		change.newWriteIDs[id] = struct{}{}
		change.newWriteEntries.addLocation(id, loc)
	})
	return change
}

func vegetaValidateBatch(
	batch []int,
	positions map[int]int,
	preTrackers []*accessTracker,
	actualTrackers []*accessTracker,
	universe vegetaUniverse,
) (map[int]struct{}, map[int]struct{}, []int, error) {
	changes := make(map[int]vegetaAccessChange, len(batch))
	candidates := make(map[int]struct{}, len(batch))
	deferred := make(map[int]struct{})
	immediate := make(map[int]struct{})
	for _, idx := range batch {
		change := classifyVegetaAccessChange(preTrackers[idx], actualTrackers[idx], universe)
		changes[idx] = change
		if change.changed {
			deferred[idx] = struct{}{}
		} else {
			candidates[idx] = struct{}{}
		}
	}

	// New keys were absent from the speculative global key dictionary. They are
	// safe unless a transaction newly reads a key/range that another currently
	// accepted transaction newly writes in the same batch. Iteratively removing
	// such readers mirrors Vegeta's new-key check while giving deferred writers a
	// deterministic final-serial position.
	for {
		newWrites := newWriteSet(8)
		writers := make(map[accessID]map[int]struct{})
		for idx := range candidates {
			for id := range changes[idx].newWriteIDs {
				if writers[id] == nil {
					writers[id] = make(map[int]struct{})
				}
				writers[id][idx] = struct{}{}
			}
			newWrites.merge(changes[idx].newWriteEntries)
		}
		var remove []int
		for idx := range candidates {
			unsafe := false
			for id := range changes[idx].newReadIDs {
				for writer := range writers[id] {
					if writer != idx {
						unsafe = true
						break
					}
				}
				if unsafe {
					break
				}
			}
			if !unsafe {
				for _, readRange := range changes[idx].newReadRanges {
					forEachWrite(&newWrites, func(_ accessID, loc writeLocation) {
						if !unsafe && loc.store == readRange.store && keyInRange(loc.key, readRange.start, readRange.end) {
							// Self-only writes are allowed, as in upstream Vegeta.
							for writer := range candidates {
								if writer == idx {
									continue
								}
								writerChange := changes[writer]
								if writeSetContainsLocation(&writerChange.newWriteEntries, exactAccessID(loc.store, loc.key), loc) {
									unsafe = true
									break
								}
							}
						}
					})
					if unsafe {
						break
					}
				}
			}
			if unsafe {
				remove = append(remove, idx)
			}
		}
		if len(remove) == 0 {
			break
		}
		for _, idx := range remove {
			delete(candidates, idx)
			immediate[idx] = struct{}{}
		}
	}

	// A new mixed dependency can appear only because the replay accessed keys
	// absent from speculation. Preserve correctness by deferring the later
	// proposal transaction instead of pretending the original DAG covered it.
	for {
		var remove = -1
		candidateList := make([]int, 0, len(candidates))
		for idx := range candidates {
			candidateList = append(candidateList, idx)
		}
		sort.Slice(candidateList, func(i, j int) bool { return positions[candidateList[i]] < positions[candidateList[j]] })
		for i := 0; i < len(candidateList) && remove < 0; i++ {
			for j := i + 1; j < len(candidateList); j++ {
				earlier, later := candidateList[i], candidateList[j]
				kinds := dependencyBetween(actualTrackers[earlier], actualTrackers[later])
				if kinds&dependencyRAW != 0 && (kinds&dependencyWAR != 0 || kinds&dependencyWAW != 0) {
					remove = later
					break
				}
			}
		}
		if remove < 0 {
			break
		}
		delete(candidates, remove)
		deferred[remove] = struct{}{}
	}

	accepted := make([]int, 0, len(candidates))
	for _, idx := range batch {
		if _, ok := candidates[idx]; ok {
			accepted = append(accepted, idx)
		}
	}
	order, err := serializationOrderForSnapshot(accepted, positions, actualTrackers)
	if err != nil {
		return nil, nil, nil, err
	}
	return deferred, immediate, order, nil
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
	return edges
}

func nextDAGWave(nodes []int, completed map[int]struct{}, edges map[int]map[int]struct{}) []int {
	incoming := make(map[int]int, len(nodes))
	for _, node := range nodes {
		if _, done := completed[node]; !done {
			incoming[node] = 0
		}
	}
	for from, tos := range edges {
		if _, fromDone := completed[from]; fromDone {
			continue
		}
		if _, active := incoming[from]; !active {
			continue
		}
		for to := range tos {
			if _, active := incoming[to]; active {
				incoming[to]++
			}
		}
	}
	wave := make([]int, 0)
	for node, degree := range incoming {
		if degree == 0 {
			wave = append(wave, node)
		}
	}
	sort.Ints(wave)
	return wave
}

func trackersConflict(indices []int, trackers []*accessTracker) bool {
	for i := 0; i < len(indices); i++ {
		for j := i + 1; j < len(indices); j++ {
			if dependencyBetween(trackers[indices[i]], trackers[indices[j]]) != 0 {
				return true
			}
		}
	}
	return false
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
	initial := speculateIndices(ctx, r.workers, ms, txs, indices, deliverTx)
	trackers := make([]*accessTracker, len(txs))
	for idx, result := range initial {
		if result.store == nil {
			return nil, fmt.Errorf("aria-fb missing initial execution tx=%d", idx)
		}
		if result.result != nil && result.result.Code != 0 {
			return nil, fmt.Errorf("aria-fb initial tx %d failed: %s", idx, result.result.Log)
		}
		trackers[idx] = result.store.tracker
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

	for _, idx := range acceptedOrder {
		initial[idx].store.Write()
		results[idx] = initial[idx].result
		r.serializationOrder = append(r.serializationOrder, idx)
	}

	fallback := make([]int, 0, len(fallbacks))
	for idx := range fallbacks {
		fallback = append(fallback, idx)
	}
	sort.Ints(fallback)
	if len(fallback) > 0 {
		replayStarted := time.Now()
		edges := ariaFallbackEdges(fallback, trackers)
		completed := make(map[int]struct{}, len(fallback))
		for len(completed) < len(fallback) {
			wave := nextDAGWave(fallback, completed, edges)
			if len(wave) == 0 {
				return nil, fmt.Errorf("aria-fb fallback DAG stalled completed=%d total=%d", len(completed), len(fallback))
			}
			waveSpec := speculateIndices(ctx, r.workers, ms, txs, wave, deliverTx)
			stats.Attempts += uint64(len(wave))
			waveTrackers := make([]*accessTracker, len(txs))
			for _, idx := range wave {
				result, ok := waveSpec[idx]
				if !ok || result.store == nil {
					return nil, fmt.Errorf("aria-fb missing fallback execution tx=%d", idx)
				}
				waveTrackers[idx] = result.store.tracker
			}
			validationStarted := time.Now()
			changedConflict := trackersConflict(wave, waveTrackers)
			stats.ValidationNanos += uint64(time.Since(validationStarted).Nanoseconds())
			if changedConflict {
				// The pre-execution DAG missed a conflict because a Wasmd execution
				// changed its concrete access set. Re-run this wave serially from the
				// current canonical state rather than committing a non-serializable batch.
				for _, idx := range wave {
					final := replayOne(ms, txs[idx], idx, deliverTx)
					stats.Attempts++
					stats.SafetyReplays++
					if final.result != nil && final.result.Code != 0 {
						return nil, fmt.Errorf("aria-fb safety replay tx %d failed: %s", idx, final.result.Log)
					}
					final.store.Write()
					results[idx] = final.result
					r.serializationOrder = append(r.serializationOrder, idx)
				}
			} else {
				for _, idx := range wave {
					result := waveSpec[idx]
					if result.result != nil && result.result.Code != 0 {
						return nil, fmt.Errorf("aria-fb fallback tx %d failed: %s", idx, result.result.Log)
					}
					result.store.Write()
					results[idx] = result.result
					r.serializationOrder = append(r.serializationOrder, idx)
				}
			}
			for _, idx := range wave {
				completed[idx] = struct{}{}
			}
		}
		stats.ReplayExecutionNanos = uint64(time.Since(replayStarted).Nanoseconds())
	}
	stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
	stats.Reexecutions = stats.Attempts - uint64(len(txs))
	r.last = stats
	return results, ctx.Err()
}

// VegetaRunner ports the attached repository's SpeculateMod + ParallelMod
// semantics to the same Wasmd/Cosmos substrate. Pre-consensus execution is used
// only to discover actual accesses, choose the hot-key proposal reorder, and
// build the dependency matrix. After consensus every transaction is replayed in
// Rule-2-compatible DAG batches; only access-set changes that cannot be safely
// committed under Vegeta's new-key rules are executed again at the end.
type VegetaRunner struct {
	workers            int
	last               policyRunStats
	proposalOrder      []int
	serializationOrder []int
}

func NewVegetaRunner(workers int) *VegetaRunner   { return &VegetaRunner{workers: workers} }
func (r *VegetaRunner) LastStats() policyRunStats { return r.last }
func (r *VegetaRunner) LastProposalOrder() []int {
	return append([]int(nil), r.proposalOrder...)
}
func (r *VegetaRunner) LastSerializationOrder() []int {
	return append([]int(nil), r.serializationOrder...)
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
	preSpec := speculateIndices(ctx, r.workers, ms, txs, indices, deliverTx)
	preTrackers := make([]*accessTracker, len(txs))
	for idx, result := range preSpec {
		if result.store == nil {
			return nil, fmt.Errorf("vegeta missing speculative execution tx=%d", idx)
		}
		if result.result != nil && result.result.Code != 0 {
			return nil, fmt.Errorf("vegeta speculative tx %d failed: %s", idx, result.result.Log)
		}
		preTrackers[idx] = result.store.tracker
	}
	analysisStarted := time.Now()
	r.proposalOrder = vegetaProposalOrder(preTrackers)
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
	deferred := make(map[int]struct{})
	postStarted := time.Now()
	for completed := 0; completed < len(r.proposalOrder); {
		batchPositions := nextVegetaBatch(matrix, done)
		if len(batchPositions) == 0 {
			return nil, fmt.Errorf("vegeta replay DAG stalled completed=%d total=%d", completed, len(r.proposalOrder))
		}
		batch := make([]int, 0, len(batchPositions))
		for _, pos := range batchPositions {
			batch = append(batch, r.proposalOrder[pos])
		}
		postSpec := speculateIndices(ctx, r.workers, ms, txs, batch, deliverTx)
		stats.Attempts += uint64(len(batch))
		actualTrackers := make([]*accessTracker, len(txs))
		for _, idx := range batch {
			result, ok := postSpec[idx]
			if !ok || result.store == nil {
				return nil, fmt.Errorf("vegeta missing replay execution tx=%d", idx)
			}
			if result.result != nil && result.result.Code != 0 {
				return nil, fmt.Errorf("vegeta replay tx %d failed: %s", idx, result.result.Log)
			}
			actualTrackers[idx] = result.store.tracker
		}
		validationStarted := time.Now()
		batchDeferred, immediateReplay, acceptedOrder, err := vegetaValidateBatch(batch, positions, preTrackers, actualTrackers, universe)
		stats.ValidationNanos += uint64(time.Since(validationStarted).Nanoseconds())
		if err != nil {
			return nil, err
		}
		for idx := range batchDeferred {
			deferred[idx] = struct{}{}
		}
		for _, idx := range acceptedOrder {
			postSpec[idx].store.Write()
			results[idx] = postSpec[idx].result
			stats.Reused++
			r.serializationOrder = append(r.serializationOrder, idx)
		}
		if len(immediateReplay) > 0 {
			replayStarted := time.Now()
			for _, idx := range r.proposalOrder {
				if _, ok := immediateReplay[idx]; !ok {
					continue
				}
				final := replayOne(ms, txs[idx], idx, deliverTx)
				stats.Attempts++
				stats.SafetyReplays++
				if final.result != nil && final.result.Code != 0 {
					return nil, fmt.Errorf("vegeta new-key replay tx %d failed: %s", idx, final.result.Log)
				}
				final.store.Write()
				results[idx] = final.result
				r.serializationOrder = append(r.serializationOrder, idx)
			}
			stats.ReplayExecutionNanos += uint64(time.Since(replayStarted).Nanoseconds())
		}
		for _, pos := range batchPositions {
			if !done[pos] {
				done[pos] = true
				completed++
			}
		}
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
				return nil, fmt.Errorf("vegeta final re-execution tx %d failed: %s", idx, final.result.Log)
			}
			final.store.Write()
			results[idx] = final.result
			r.serializationOrder = append(r.serializationOrder, idx)
		}
		stats.ReplayExecutionNanos += uint64(time.Since(replayStarted).Nanoseconds())
	}
	stats.PostConsensusNanos = uint64(time.Since(postStarted).Nanoseconds())
	// Upstream ParallelMod reports needReexecute (known access-set changes) as
	// its re-execution count; immediate new-key safety replays are reported
	// separately here as SafetyReplays.
	stats.Reexecutions = uint64(len(deferred))
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
