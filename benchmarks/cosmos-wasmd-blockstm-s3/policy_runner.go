package main

import (
	"bytes"
	"context"
	"fmt"
	"sort"
	"sync"
	"sync/atomic"

	abci "github.com/cometbft/cometbft/abci/types"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

// policyRunStats records actual execution attempts. An attempt beyond the
// transaction count is a validation-triggered replay.
type policyRunStats struct {
	Attempts     uint64
	Reexecutions uint64
	Speculated   uint64
	Reused       uint64
	Replayed     uint64
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
	ids map[storetypes.StoreKey]storeID
}

func newStoreIDRegistry() *storeIDRegistry {
	return &storeIDRegistry{ids: make(map[storetypes.StoreKey]storeID, 16)}
}

func (r *storeIDRegistry) id(key storetypes.StoreKey) storeID {
	if id, ok := r.ids[key]; ok {
		return id
	}
	id := storeIDFromName(key.Name())
	r.ids[key] = id
	return id
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
	return append([]byte(nil), bz...)
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
	store   storeID
	tracker *accessTracker
}

func (s trackingKVStore) Get(key []byte) []byte {
	s.tracker.read(s.store, key)
	return s.KVStore.Get(key)
}
func (s trackingKVStore) Has(key []byte) bool {
	s.tracker.read(s.store, key)
	return s.KVStore.Has(key)
}
func (s trackingKVStore) Set(key, value []byte) {
	s.tracker.write(s.store, key)
	s.KVStore.Set(key, value)
}
func (s trackingKVStore) Delete(key []byte) {
	s.tracker.write(s.store, key)
	s.KVStore.Delete(key)
}
func (s trackingKVStore) Iterator(start, end []byte) storetypes.Iterator {
	s.tracker.readRange(s.store, start, end)
	return s.KVStore.Iterator(start, end)
}
func (s trackingKVStore) ReverseIterator(start, end []byte) storetypes.Iterator {
	s.tracker.readRange(s.store, start, end)
	return s.KVStore.ReverseIterator(start, end)
}

// ObjKVStore is generic over any in store/v2. Track it as well so scheduler
// correctness does not depend on a keeper choosing byte-backed vs object stores.
type trackingObjKVStore struct {
	storetypes.ObjKVStore
	store   storeID
	tracker *accessTracker
}

func (s trackingObjKVStore) Get(key []byte) any {
	s.tracker.read(s.store, key)
	return s.ObjKVStore.Get(key)
}
func (s trackingObjKVStore) Has(key []byte) bool {
	s.tracker.read(s.store, key)
	return s.ObjKVStore.Has(key)
}
func (s trackingObjKVStore) Set(key []byte, value any) {
	s.tracker.write(s.store, key)
	s.ObjKVStore.Set(key, value)
}
func (s trackingObjKVStore) Delete(key []byte) {
	s.tracker.write(s.store, key)
	s.ObjKVStore.Delete(key)
}
func (s trackingObjKVStore) Iterator(start, end []byte) storetypes.ObjIterator {
	s.tracker.readRange(s.store, start, end)
	return s.ObjKVStore.Iterator(start, end)
}
func (s trackingObjKVStore) ReverseIterator(start, end []byte) storetypes.ObjIterator {
	s.tracker.readRange(s.store, start, end)
	return s.ObjKVStore.ReverseIterator(start, end)
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
	tracker *accessTracker
	stores  *storeIDRegistry
}

func wrapTrackingCacheMultiStore(store storetypes.CacheMultiStore, parent *accessTracker, stores *storeIDRegistry) *trackingMultiStore {
	return &trackingMultiStore{
		cacheMultiStoreDelegate: cacheMultiStoreDelegate{CacheMultiStore: store},
		tracker:                 newAccessTracker(parent),
		stores:                  stores,
	}
}

func newTrackingMultiStore(parent storetypes.MultiStore) *trackingMultiStore {
	return wrapTrackingCacheMultiStore(parent.CacheMultiStore(), nil, newStoreIDRegistry())
}

func (m *trackingMultiStore) CacheWrap() storetypes.CacheWrap { return m.CacheMultiStore() }

func (m *trackingMultiStore) CacheMultiStore() storetypes.CacheMultiStore {
	child := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStore()
	return wrapTrackingCacheMultiStore(child, m.tracker, m.stores)
}

func (m *trackingMultiStore) CacheMultiStoreWithVersion(version int64) (storetypes.CacheMultiStore, error) {
	child, err := m.cacheMultiStoreDelegate.CacheMultiStore.CacheMultiStoreWithVersion(version)
	if err != nil {
		return nil, err
	}
	return wrapTrackingCacheMultiStore(child, m.tracker, m.stores), nil
}

func (m *trackingMultiStore) GetStore(key storetypes.StoreKey) storetypes.Store {
	store := m.cacheMultiStoreDelegate.CacheMultiStore.GetStore(key)
	if kv, ok := store.(storetypes.KVStore); ok {
		return trackingKVStore{KVStore: kv, store: m.stores.id(key), tracker: m.tracker}
	}
	if obj, ok := store.(storetypes.ObjKVStore); ok {
		return trackingObjKVStore{ObjKVStore: obj, store: m.stores.id(key), tracker: m.tracker}
	}
	return store
}

func (m *trackingMultiStore) GetKVStore(key storetypes.StoreKey) storetypes.KVStore {
	return trackingKVStore{
		KVStore: m.cacheMultiStoreDelegate.CacheMultiStore.GetKVStore(key),
		store:   m.stores.id(key),
		tracker: m.tracker,
	}
}

func (m *trackingMultiStore) GetObjKVStore(key storetypes.StoreKey) storetypes.ObjKVStore {
	return trackingObjKVStore{
		ObjKVStore: m.cacheMultiStoreDelegate.CacheMultiStore.GetObjKVStore(key),
		store:      m.stores.id(key),
		tracker:    m.tracker,
	}
}

func (m *trackingMultiStore) Write() {
	m.cacheMultiStoreDelegate.CacheMultiStore.Write()
	m.tracker.mergeIntoParent()
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
		if readsConflictWithWrites(r.store.tracker, priorWrites) {
			stats.Replayed++
			r = replayOne(ms, txs[idx], idx, deliverTx)
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

// VegetaRunner is a Wasmd/Cosmos port of Vegeta's speculate-order-replay
// concurrency-control shape: execute the block speculatively, derive actual
// dependencies from execution, then replay invalidated transactions in original
// deterministic order. It is intentionally implemented over the same SDK cache
// store and deliver closure used by the other Wasmd baselines.
type VegetaRunner struct {
	workers int
	last    policyRunStats
}

func NewVegetaRunner(workers int) *VegetaRunner   { return &VegetaRunner{workers: workers} }
func (r *VegetaRunner) LastStats() policyRunStats { return r.last }

func (r *VegetaRunner) Run(ctx context.Context, ms storetypes.MultiStore, txs [][]byte, deliverTx sdk.DeliverTxFunc) ([]*abci.ExecTxResult, error) {
	indices := make([]int, len(txs))
	for i := range indices {
		indices[i] = i
	}
	results := make([]*abci.ExecTxResult, len(txs))
	if len(indices) == 0 {
		r.last = policyRunStats{}
		return results, nil
	}
	stats := policyRunStats{Speculated: uint64(len(indices))}
	spec := speculateIndices(ctx, r.workers, ms, txs, indices, deliverTx)
	var attempts atomic.Uint64
	priorWrites := newWriteSet(64)
	if err := commitSpeculation(ms, txs, indices, spec, deliverTx, results, &priorWrites, &attempts, &stats); err != nil {
		return nil, err
	}
	a := attempts.Load()
	stats.Attempts = a
	stats.Reexecutions = a - uint64(len(txs))
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
