package main

import (
	"context"
	"fmt"
	"sort"
	"strings"
	"sync"
	"time"

	abci "github.com/cometbft/cometbft/abci/types"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

const (
	conflictReadWrite  uint8 = 1 << 0
	conflictWriteRead  uint8 = 1 << 1
	conflictWriteWrite uint8 = 1 << 2
)

type rustTxDeltaEntry struct {
	storeKey storetypes.StoreKey
	key      []byte
	object   bool
	deleted  bool
	bytes    []byte
	value    any
}

type rustTxDelta struct {
	entries []rustTxDeltaEntry
}

type rustReadyReceipt struct {
	index          int
	store          *trackingMultiStore
	result         *abci.ExecTxResult
	delta          rustTxDelta
	visible        rustVisibilityMask
	startedNanos   uint64
	completedNanos uint64
	executionNanos uint64
	deltaNanos     uint64
}

type rustReadyJob struct {
	index        int
	store        *trackingMultiStore
	visible      rustVisibilityMask
	startedNanos uint64
}

type rustReplayCause struct {
	predecessor int
	transaction int
	kinds       uint8
}

// RustSymbGraphRunner executes the dependency DAG emitted by crates/acg-*.
// Scheduler levels are diagnostics only: a successor becomes runnable as soon
// as all of its explicit predecessors complete.
type RustSymbGraphRunner struct {
	workers            int
	block              ExecutionBlock
	bridge             *RustSymbGraphBridge
	estimatedCosts     []uint32
	serialServiceNanos uint64
	options            RustSymbGraphRunnerOptions
	fixedPlan          *rustPlanResponse
	fixedPlanNanos     uint64
	feedbackEnabled    bool
	requireZeroReplay  bool
	variantOverride    string
	last               policyRunStats
	lastPlan           rustPlanResponse
	lastFeedback       rustFeedbackResponse
	lastDiagnostics    RustSymbGraphDiagnostics
}

func NewRustSymbGraphRunner(workers int, block ExecutionBlock, bridge *RustSymbGraphBridge, estimatedCosts []uint32) *RustSymbGraphRunner {
	return NewRustSymbGraphRunnerWithOptions(workers, block, bridge, estimatedCosts, DefaultRustSymbGraphRunnerOptions())
}

func NewRustSymbGraphRunnerWithOptions(workers int, block ExecutionBlock, bridge *RustSymbGraphBridge, estimatedCosts []uint32, options RustSymbGraphRunnerOptions) *RustSymbGraphRunner {
	if workers < 1 {
		workers = 1
	}
	normalized, err := options.Normalize()
	if err != nil {
		panic(err)
	}
	return &RustSymbGraphRunner{
		workers:         workers,
		block:           block,
		bridge:          bridge,
		estimatedCosts:  append([]uint32(nil), estimatedCosts...),
		options:         normalized,
		feedbackEnabled: true,
	}
}

// NewRustSymbGraphExactTraceOracleRunner reuses the production Rust-ACG
// execution, MVCC visibility, canonical validation, delta reuse, and replay
// machinery with a hindsight exact-access plan. Only the symbolic prediction
// source is replaced. Runtime feedback is disabled because perfect analysis
// does not need learning, and any replay is a harness failure.
func NewRustSymbGraphExactTraceOracleRunner(workers int, block ExecutionBlock, estimatedCosts []uint32, plan rustPlanResponse, planNanos uint64, options RustSymbGraphRunnerOptions) *RustSymbGraphRunner {
	r := NewRustSymbGraphRunnerWithOptions(workers, block, nil, estimatedCosts, options)
	planCopy := plan
	planCopy.Dependencies = append([]rustPlanDependency(nil), plan.Dependencies...)
	planCopy.FeedbackPairs = append([]rustPlanPair(nil), plan.FeedbackPairs...)
	r.fixedPlan = &planCopy
	r.fixedPlanNanos = planNanos
	r.feedbackEnabled = false
	r.requireZeroReplay = true
	r.variantOverride = "exact-ethereum-trace+" + r.options.Variant()
	return r
}

func (r *RustSymbGraphRunner) SetSerialServiceNanos(nanos uint64) { r.serialServiceNanos = nanos }
func (r *RustSymbGraphRunner) LastStats() policyRunStats          { return r.last }
func (r *RustSymbGraphRunner) LastPlan() rustPlanResponse         { return r.lastPlan }
func (r *RustSymbGraphRunner) LastFeedback() rustFeedbackResponse { return r.lastFeedback }
func (r *RustSymbGraphRunner) LastDiagnostics() RustSymbGraphDiagnostics {
	return r.lastDiagnostics
}

func collectWriteLocations(writes writeSet) []writeLocation {
	out := make([]writeLocation, 0, len(writes.exact))
	for _, loc := range writes.exact {
		out = append(out, loc)
	}
	for _, collisions := range writes.collisions {
		out = append(out, collisions...)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].store != out[j].store {
			return out[i].store < out[j].store
		}
		return string(out[i].key) < string(out[j].key)
	})
	return out
}

func captureRustDelta(store *trackingMultiStore) (rustTxDelta, error) {
	locations := collectWriteLocations(store.tracker.writes)
	delta := rustTxDelta{entries: make([]rustTxDeltaEntry, 0, len(locations))}
	for _, loc := range locations {
		storeKey, ok := store.stores.key(loc.store)
		if !ok {
			return rustTxDelta{}, fmt.Errorf("missing concrete StoreKey for tracked store=%d", loc.store)
		}
		base := store.cacheMultiStoreDelegate.CacheMultiStore.GetStore(storeKey)
		switch typed := base.(type) {
		case storetypes.KVStore:
			exists := typed.Has(loc.key)
			entry := rustTxDeltaEntry{storeKey: storeKey, key: cloneBytes(loc.key), deleted: !exists}
			if exists {
				entry.bytes = cloneBytes(typed.Get(loc.key))
			}
			delta.entries = append(delta.entries, entry)
		case storetypes.ObjKVStore:
			exists := typed.Has(loc.key)
			entry := rustTxDeltaEntry{storeKey: storeKey, key: cloneBytes(loc.key), object: true, deleted: !exists}
			if exists {
				entry.value = typed.Get(loc.key)
			}
			delta.entries = append(delta.entries, entry)
		default:
			return rustTxDelta{}, fmt.Errorf("tracked write store %s is neither KVStore nor ObjKVStore", storeKey.Name())
		}
	}
	return delta, nil
}

func applyRustDelta(ms storetypes.MultiStore, delta rustTxDelta) error {
	for _, entry := range delta.entries {
		store := ms.GetStore(entry.storeKey)
		if entry.object {
			typed, ok := store.(storetypes.ObjKVStore)
			if !ok {
				return fmt.Errorf("store %s stopped implementing ObjKVStore", entry.storeKey.Name())
			}
			if entry.deleted {
				typed.Delete(entry.key)
			} else {
				typed.Set(entry.key, entry.value)
			}
			continue
		}
		typed, ok := store.(storetypes.KVStore)
		if !ok {
			return fmt.Errorf("store %s stopped implementing KVStore", entry.storeKey.Name())
		}
		if entry.deleted {
			typed.Delete(entry.key)
		} else {
			typed.Set(entry.key, entry.bytes)
		}
	}
	return nil
}

func writesOverlap(left, right *writeSet) bool {
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

func rustConflictKinds(left, right *accessTracker) uint8 {
	var kinds uint8
	if readsConflictWithWrites(left, &right.writes) {
		kinds |= conflictReadWrite
	}
	if readsConflictWithWrites(right, &left.writes) {
		kinds |= conflictWriteRead
	}
	if writesOverlap(&left.writes, &right.writes) {
		kinds |= conflictWriteWrite
	}
	return kinds
}

func rustPairObservations(trackers []*accessTracker, source string, include func(i, j int) bool) []rustPairObservation {
	var out []rustPairObservation
	for i := 0; i < len(trackers); i++ {
		if trackers[i] == nil {
			continue
		}
		for j := i + 1; j < len(trackers); j++ {
			if trackers[j] == nil || !include(i, j) {
				continue
			}
			kinds := rustConflictKinds(trackers[i], trackers[j])
			out = append(out, rustPairObservation{Left: i, Right: j, ConflictKinds: kinds, Conflict: kinds != 0, Source: source})
		}
	}
	return out
}

func rustPairObservationsForPlanPairs(trackers []*accessTracker, pairs []rustPlanPair, source string, include func(i, j int) bool) []rustPairObservation {
	out := make([]rustPairObservation, 0, len(pairs))
	for _, pair := range pairs {
		i, j := pair.Left, pair.Right
		if i < 0 || j < 0 || i >= len(trackers) || j >= len(trackers) || i >= j || trackers[i] == nil || trackers[j] == nil || !include(i, j) {
			continue
		}
		kinds := rustConflictKinds(trackers[i], trackers[j])
		out = append(out, rustPairObservation{Left: i, Right: j, ConflictKinds: kinds, Conflict: kinds != 0, Source: source})
	}
	return out
}

func rustDependencyTopology(count int, dependencies []rustPlanDependency) ([][]int, [][]int, []int, error) {
	preds := make([][]int, count)
	succs := make([][]int, count)
	indegree := make([]int, count)
	seen := map[[2]int]struct{}{}
	for _, dependency := range dependencies {
		p, s := dependency.Predecessor, dependency.Successor
		if p < 0 || p >= count || s < 0 || s >= count || p >= s {
			return nil, nil, nil, fmt.Errorf("invalid Rust dependency %d -> %d for %d transactions", p, s, count)
		}
		key := [2]int{p, s}
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		preds[s] = append(preds[s], p)
		succs[p] = append(succs[p], s)
		indegree[s]++
	}
	for i := range preds {
		sort.Ints(preds[i])
		sort.Ints(succs[i])
	}
	return preds, succs, indegree, nil
}

func recordRustReady(diag *RustSymbGraphDiagnostics, ready int) {
	diag.ReadySamples++
	diag.ReadySum += uint64(ready)
	if ready > diag.MaxReady {
		diag.MaxReady = ready
	}
}

func (r *RustSymbGraphRunner) preexecuteReadyDAG(
	ctx context.Context,
	ms storetypes.MultiStore,
	txs [][]byte,
	plan rustPlanResponse,
	deliverTx sdk.DeliverTxFunc,
	diag *RustSymbGraphDiagnostics,
) ([]rustReadyReceipt, []uint64, time.Duration, error) {
	count := len(txs)
	_, succs, indegree, err := rustDependencyTopology(count, plan.Dependencies)
	if err != nil {
		return nil, nil, 0, err
	}
	if count == 0 {
		return nil, nil, 0, nil
	}
	workers := r.workers
	if workers > count {
		workers = count
	}
	phaseStart := time.Now()
	jobs := make(chan rustReadyJob)
	results := make(chan rustReadyReceipt, workers)
	var wg sync.WaitGroup
	for worker := 0; worker < workers; worker++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for job := range jobs {
				executionStarted := time.Now()
				res := deliverTx(txs[job.index], nil, job.store, job.index, map[string]any{})
				executionNanos := uint64(time.Since(executionStarted).Nanoseconds())
				deltaStarted := time.Now()
				delta, deltaErr := captureRustDelta(job.store)
				deltaNanos := uint64(time.Since(deltaStarted).Nanoseconds())
				if deltaErr != nil && (res == nil || res.Code == 0) {
					res = &abci.ExecTxResult{Code: 1, Log: deltaErr.Error()}
				}
				completed := uint64(time.Since(phaseStart).Nanoseconds())
				results <- rustReadyReceipt{
					index:          job.index,
					store:          job.store,
					result:         res,
					delta:          delta,
					visible:        job.visible,
					startedNanos:   job.startedNanos,
					completedNanos: completed,
					executionNanos: executionNanos,
					deltaNanos:     deltaNanos,
				}
			}
		}()
	}

	ready := make([]int, 0, count)
	for i, degree := range indegree {
		if degree == 0 {
			ready = append(ready, i)
		}
	}
	sort.Ints(ready)
	diag.InitialReady = len(ready)
	recordRustReady(diag, len(ready))
	completedWords := make([]uint64, (count+63)/64)
	completedSuccess := make([]bool, count)
	receipts := make([]rustReadyReceipt, count)
	completedAt := make([]uint64, count)
	versions := newRustBlockMVCC()
	active, completedCount := 0, 0

	for completedCount < count {
		for active < workers && len(ready) != 0 {
			select {
			case <-ctx.Done():
				close(jobs)
				wg.Wait()
				return nil, nil, time.Since(phaseStart), ctx.Err()
			default:
			}
			idx := ready[0]
			ready = ready[1:]
			visibilityStarted := time.Now()
			visible := rustVisibilityFromCompleted(completedWords, idx)
			diag.VisibilityNanos += uint64(time.Since(visibilityStarted).Nanoseconds())

			branchStarted := time.Now()
			var branch *trackingMultiStore
			if r.options.Visibility == rustVisibilityMVCC {
				branch = newTrackingMultiStoreWithMVCC(ms, &rustMvccReadView{versions: versions, canonicalIndex: idx, visibility: visible})
			} else {
				branch = newTrackingMultiStore(ms)
			}
			diag.BranchCreateNanos += uint64(time.Since(branchStarted).Nanoseconds())

			if r.options.Visibility == rustVisibilityMaterialized {
				materializeStarted := time.Now()
				for prior := 0; prior < idx; prior++ {
					if !completedSuccess[prior] || !visible.contains(prior) {
						continue
					}
					if err := applyRustDelta(branch.cacheMultiStoreDelegate.CacheMultiStore, receipts[prior].delta); err != nil {
						close(jobs)
						wg.Wait()
						return nil, nil, time.Since(phaseStart), fmt.Errorf("materialize launch-visible tx %d into tx %d: %w", prior, idx, err)
					}
				}
				diag.VisibilityNanos += uint64(time.Since(materializeStarted).Nanoseconds())
			}

			started := uint64(time.Since(phaseStart).Nanoseconds())
			jobs <- rustReadyJob{index: idx, store: branch, visible: visible, startedNanos: started}
			active++
			if active > diag.MaxActive {
				diag.MaxActive = active
			}
			recordRustReady(diag, len(ready))
		}
		if active == 0 {
			close(jobs)
			wg.Wait()
			return nil, nil, time.Since(phaseStart), fmt.Errorf("Rust dependency DAG made no progress")
		}
		receipt := <-results
		active--
		completedCount++
		receipts[receipt.index] = receipt
		completedAt[receipt.index] = receipt.completedNanos
		diag.SpecExecutionNanos += receipt.executionNanos
		diag.DeltaCaptureNanos += receipt.deltaNanos
		speculativeSucceeded := receipt.result == nil || receipt.result.Code == 0
		// A contract-level speculative failure is still a valid receipt. It may
		// have observed stale state and must survive until canonical validation,
		// which can replay it against all earlier committed writes. This mirrors
		// the production Rust engine: failed receipts are not published into MVCC
		// visibility, but they do complete their DAG node and unblock successors.
		if speculativeSucceeded && r.options.Visibility == rustVisibilityMVCC {
			publishStarted := time.Now()
			versions.publish(receipt.index, receipt.delta)
			diag.MVCCPublishNanos += uint64(time.Since(publishStarted).Nanoseconds())
		}
		if speculativeSucceeded {
			completedSuccess[receipt.index] = true
			markRustCompleted(completedWords, receipt.index)
		}
		for _, successor := range succs[receipt.index] {
			indegree[successor]--
			if indegree[successor] == 0 {
				ready = append(ready, successor)
				sort.Ints(ready)
			}
		}
		recordRustReady(diag, len(ready))
	}
	close(jobs)
	wg.Wait()
	elapsed := time.Since(phaseStart)
	if r.options.Visibility == rustVisibilityMVCC {
		mvcc := versions.diagnostics()
		diag.MVCCPointReads = mvcc.PointReads
		diag.MVCCVersionHits = mvcc.VersionHits
		diag.MVCCBaseFallbacks = mvcc.BaseFallbacks
		diag.MVCCRangeReads = mvcc.RangeReads
		diag.MVCCRangeOverlayKeys = mvcc.RangeKeys
		diag.MVCCPublishes = mvcc.Publishes
		diag.MVCCPublishedKeys = mvcc.PublishedKeys
	}
	busy := diag.SpecExecutionNanos + diag.DeltaCaptureNanos
	capacity := uint64(elapsed.Nanoseconds()) * uint64(workers)
	if capacity != 0 {
		diag.WorkerUtilization = float64(busy) / float64(capacity)
		if busy < capacity {
			diag.WorkerIdleNanos = capacity - busy
		}
	}
	return receipts, completedAt, elapsed, nil
}

func rustSerializationFeedback(plan rustPlanResponse, completedAt []uint64) []rustSerializationObservation {
	preds, _, _, err := rustDependencyTopology(plan.TransactionCount, plan.Dependencies)
	if err != nil {
		return nil
	}
	out := make([]rustSerializationObservation, 0, len(plan.Dependencies))
	for _, dependency := range plan.Dependencies {
		alternate := uint64(0)
		for _, predecessor := range preds[dependency.Successor] {
			if predecessor == dependency.Predecessor {
				continue
			}
			if completedAt[predecessor] > alternate {
				alternate = completedAt[predecessor]
			}
		}
		completed := completedAt[dependency.Predecessor]
		marginal := uint64(0)
		if completed > alternate {
			marginal = completed - alternate
		}
		out = append(out, rustSerializationObservation{Predecessor: dependency.Predecessor, Transaction: dependency.Successor, MarginalReadyDelayNanos: marginal})
	}
	return out
}

func replayDescendants(causes [][]int, root int) uint32 {
	seen := make([]bool, len(causes))
	queue := []int{root}
	seen[root] = true
	count := uint32(0)
	for len(queue) != 0 {
		current := queue[0]
		queue = queue[1:]
		for tx := current + 1; tx < len(causes); tx++ {
			if seen[tx] {
				continue
			}
			for _, predecessor := range causes[tx] {
				if predecessor == current {
					seen[tx] = true
					queue = append(queue, tx)
					count++
					break
				}
			}
		}
	}
	return count
}

func (r *RustSymbGraphRunner) reconcile(
	ms storetypes.MultiStore,
	txs [][]byte,
	receipts []rustReadyReceipt,
	deliverTx sdk.DeliverTxFunc,
	diag *RustSymbGraphDiagnostics,
) ([]*abci.ExecTxResult, []*accessTracker, []bool, [][]int, []uint64, time.Duration, error) {
	started := time.Now()
	results := make([]*abci.ExecTxResult, len(txs))
	finalTrackers := make([]*accessTracker, len(txs))
	replayed := make([]bool, len(txs))
	causes := make([][]int, len(txs))
	replayNanos := make([]uint64, len(txs))
	writeIndex := newRustCanonicalWriteIndex()

	for idx := range txs {
		receipt := receipts[idx]
		validationStarted := time.Now()
		var invalidating []int
		if r.options.Validation == rustValidationIndexed {
			invalidating = writeIndex.invalidating(idx, receipt.store.tracker, receipt.visible, replayed)
		} else {
			invalidating = rustInvalidatingByScan(idx, receipt, finalTrackers, replayed)
		}
		diag.ValidationNanos += uint64(time.Since(validationStarted).Nanoseconds())

		speculativeFailed := receipt.result != nil && receipt.result.Code != 0
		if len(invalidating) == 0 && !speculativeFailed {
			if err := applyRustDelta(ms, receipt.delta); err != nil {
				return nil, nil, nil, nil, nil, time.Since(started), fmt.Errorf("apply reused tx %d delta: %w", idx, err)
			}
			results[idx] = receipt.result
			finalTrackers[idx] = receipt.store.tracker
			writeIndex.add(idx, receipt.store.tracker.writes)
			continue
		}

		replayed[idx] = true
		causes[idx] = invalidating
		replayStarted := time.Now()
		branch := newTrackingMultiStore(ms)
		res := deliverTx(txs[idx], nil, branch, idx, map[string]any{})
		if res != nil && res.Code != 0 {
			return nil, nil, nil, nil, nil, time.Since(started), fmt.Errorf("replayed tx %d failed: %s", idx, res.Log)
		}
		delta, err := captureRustDelta(branch)
		if err != nil {
			return nil, nil, nil, nil, nil, time.Since(started), fmt.Errorf("capture replayed tx %d delta: %w", idx, err)
		}
		if err := applyRustDelta(ms, delta); err != nil {
			return nil, nil, nil, nil, nil, time.Since(started), fmt.Errorf("apply replayed tx %d delta: %w", idx, err)
		}
		replayNanos[idx] = uint64(time.Since(replayStarted).Nanoseconds())
		diag.ReplayExecutionNanos += replayNanos[idx]
		results[idx] = res
		finalTrackers[idx] = branch.tracker
		writeIndex.add(idx, branch.tracker.writes)
	}
	return results, finalTrackers, replayed, causes, replayNanos, time.Since(started), nil
}

func buildReplayAttributions(finalTrackers []*accessTracker, causes [][]int, replayNanos []uint64) []rustReplayAttribution {
	var out []rustReplayAttribution
	for tx, predecessors := range causes {
		if len(predecessors) == 0 {
			continue
		}
		base := replayNanos[tx] / uint64(len(predecessors))
		rem := replayNanos[tx] % uint64(len(predecessors))
		descendants := replayDescendants(causes, tx)
		for offset, predecessor := range predecessors {
			cost := base
			if uint64(offset) < rem {
				cost++
			}
			kinds := rustConflictKinds(finalTrackers[predecessor], finalTrackers[tx])
			if kinds == 0 {
				// Canonical invalidation is a predecessor-write/successor-read dependency.
				kinds = conflictWriteRead
			}
			out = append(out, rustReplayAttribution{Predecessor: predecessor, Transaction: tx, ConflictKinds: kinds, ReplayCostNanos: cost, InvalidatedDescendants: descendants})
		}
	}
	return out
}

func (r *RustSymbGraphRunner) Run(ctx context.Context, ms storetypes.MultiStore, txs [][]byte, deliverTx sdk.DeliverTxFunc) ([]*abci.ExecTxResult, error) {
	if len(txs) != len(r.block.Transactions) {
		return nil, fmt.Errorf("Rust SymbGraph block/tx mismatch: plan=%d runner=%d", len(r.block.Transactions), len(txs))
	}
	variant := r.options.Variant()
	if r.variantOverride != "" {
		variant = r.variantOverride
	}
	diag := RustSymbGraphDiagnostics{Variant: variant}
	var plan rustPlanResponse
	var planningElapsed time.Duration
	if r.fixedPlan != nil {
		plan = *r.fixedPlan
		planningElapsed = time.Duration(r.fixedPlanNanos)
	} else {
		if r.bridge == nil {
			return nil, fmt.Errorf("Rust SymbGraph runner has neither bridge nor fixed plan")
		}
		planningStarted := time.Now()
		var err error
		plan, err = r.bridge.Plan(r.block, r.estimatedCosts)
		if err != nil {
			return nil, err
		}
		planningElapsed = time.Since(planningStarted)
		diag.PlanRequestBuildNanos = plan.BridgeTimings.RequestBuildNanos
		diag.PlanRequestMarshalNanos = plan.BridgeTimings.RequestMarshalNanos
		diag.PlanCGORoundTripNanos = plan.BridgeTimings.CGORoundTripNanos
		diag.PlanResponseUnmarshalNanos = plan.BridgeTimings.ResponseUnmarshalNanos
		if plan.PlanningTimings != nil {
			diag.PlanRustDecodeNanos = plan.PlanningTimings.RustDecodeNanos
			diag.PlanResolveComponentsNanos = plan.PlanningTimings.ResolveComponentsNanos
			diag.PlanCandidateGraphNanos = plan.PlanningTimings.CandidateGraphNanos
			diag.PlanSchedulerNanos = plan.PlanningTimings.SchedulerNanos
			diag.PlanProjectionNanos = plan.PlanningTimings.ProjectionNanos
			diag.PlanFeedbackPairsNanos = plan.PlanningTimings.FeedbackPairsNanos
			diag.PlanFinalizeNanos = plan.PlanningTimings.FinalizeNanos
		}
	}
	diag.PlanNanos = uint64(planningElapsed.Nanoseconds())
	knownPlan := diag.PlanRequestBuildNanos + diag.PlanRequestMarshalNanos + diag.PlanResponseUnmarshalNanos +
		diag.PlanRustDecodeNanos + diag.PlanResolveComponentsNanos + diag.PlanCandidateGraphNanos +
		diag.PlanSchedulerNanos + diag.PlanProjectionNanos + diag.PlanFeedbackPairsNanos + diag.PlanFinalizeNanos
	if diag.PlanNanos > knownPlan {
		diag.PlanBridgeOtherNanos = diag.PlanNanos - knownPlan
	}
	if plan.TransactionCount != len(txs) {
		return nil, fmt.Errorf("Rust ACG returned transaction_count=%d want=%d", plan.TransactionCount, len(txs))
	}
	diag.DependencyEdges = len(plan.Dependencies)
	diag.FeedbackPairs = len(plan.FeedbackPairs)
	diag.PhysicalCandidateEdges = plan.CandidateEdges
	diag.LogicalCandidateEdges = plan.LogicalCandidateEdges
	diag.CompactCandidateGroups = plan.CompactCandidateGroups
	diag.ParentDependenciesBeforeReduction = plan.ParentDependenciesBeforeReduction
	diag.ParentDependenciesElidedReduction = plan.ParentDependenciesElidedByReduction
	diag.CandidateHard = plan.CandidateDecisions.Hard
	diag.CandidateSoft = plan.CandidateDecisions.Soft
	diag.CandidateLow = plan.CandidateDecisions.Low
	diag.OrderedHard = plan.CandidateDecisions.OrderedHard
	diag.OrderedSoft = plan.CandidateDecisions.OrderedSoft
	diag.CriticalPathTx, diag.CriticalPathCost, diag.CriticalPath = rustCriticalPathDetail(len(txs), plan.Dependencies, r.estimatedCosts)
	if len(plan.DependencyReasons) != 0 {
		diag.DependencyReasons, diag.DependencyPrimary, diag.CriticalPathReasons, diag.CriticalPathCostByReason = rustDependencyReasonDiagnostics(plan, diag.CriticalPath, r.estimatedCosts)
		diag.DependencyProvenance, diag.CriticalPathProvenance = rustDependencyDimensionDiagnostics(plan, diag.CriticalPath, "provenance")
		diag.DependencyDecisions, diag.CriticalPathDecisions = rustDependencyDimensionDiagnostics(plan, diag.CriticalPath, "decision")
	}
	for _, cost := range r.estimatedCosts {
		if cost == 0 {
			diag.TotalEstimatedCost++
		} else {
			diag.TotalEstimatedCost += uint64(cost)
		}
	}
	if diag.CriticalPathCost != 0 {
		diag.DAGParallelism = float64(diag.TotalEstimatedCost) / float64(diag.CriticalPathCost)
	}
	r.lastPlan = plan

	receipts, completedAt, preexecutionElapsed, err := r.preexecuteReadyDAG(ctx, ms, txs, plan, deliverTx, &diag)
	if err != nil {
		return nil, err
	}
	diag.PreexecutionNanos = uint64(preexecutionElapsed.Nanoseconds())

	var preObservations []rustPairObservation
	var serialization []rustSerializationObservation
	if r.feedbackEnabled {
		feedbackBuildStarted := time.Now()
		preTrackers := make([]*accessTracker, len(receipts))
		for i := range receipts {
			preTrackers[i] = receipts[i].store.tracker
		}
		if r.options.Feedback == rustFeedbackProfile {
			preObservations = rustPairObservationsForPlanPairs(preTrackers, plan.FeedbackPairs, "pre_execution", func(_, _ int) bool { return true })
		} else {
			preObservations = rustPairObservations(preTrackers, "pre_execution", func(_, _ int) bool { return true })
		}
		serialization = rustSerializationFeedback(plan, completedAt)
		diag.FeedbackBuildNanos += uint64(time.Since(feedbackBuildStarted).Nanoseconds())
	}

	results, finalTrackers, replayed, causes, replayNanos, reconciliationElapsed, err := r.reconcile(ms, txs, receipts, deliverTx, &diag)
	if err != nil {
		return nil, err
	}
	diag.ReconciliationNanos = uint64(reconciliationElapsed.Nanoseconds())
	diag.OracleConflictEdges, diag.OracleCriticalPathTx, diag.OracleCriticalPathCost, diag.OracleDAGParallelism, diag.OracleCriticalPath = rustActualConflictOracle(finalTrackers, r.estimatedCosts)
	if diag.DAGParallelism != 0 {
		diag.SerializationGap = diag.OracleDAGParallelism / diag.DAGParallelism
	}

	replayCount := uint64(0)
	for _, value := range replayed {
		if value {
			replayCount++
		}
	}
	if r.requireZeroReplay && replayCount != 0 {
		details := make([]string, 0, replayCount)
		for tx, predecessors := range causes {
			if len(predecessors) == 0 {
				continue
			}
			hash := ""
			if tx < len(r.block.Transactions) {
				hash = r.block.Transactions[tx].TxHash
			}
			details = append(details, fmt.Sprintf("tx=%d hash=%s invalidated_by=%v", tx, hash, predecessors))
		}
		return nil, fmt.Errorf("exact-trace Rust-ACG oracle replayed %d/%d transactions in block %d after translation compensation; perfect-access contract violated: %s", replayCount, len(txs), r.block.BlockNumber, strings.Join(details, "; "))
	}

	if r.feedbackEnabled {
		feedbackBuildStarted := time.Now()
		var replayObservations []rustPairObservation
		if r.options.Feedback == rustFeedbackProfile {
			replayObservations = rustPairObservationsForPlanPairs(finalTrackers, plan.FeedbackPairs, "replay", func(i, j int) bool { return replayed[i] || replayed[j] })
		} else {
			replayObservations = rustPairObservations(finalTrackers, "replay", func(i, j int) bool { return replayed[i] || replayed[j] })
		}
		observations := append(preObservations, replayObservations...)
		replayAttributions := buildReplayAttributions(finalTrackers, causes, replayNanos)
		diag.FeedbackBuildNanos += uint64(time.Since(feedbackBuildStarted).Nanoseconds())

		rustFeedbackStarted := time.Now()
		feedback, err := r.bridge.Feedback(rustFeedbackRequest{
			Epoch:              r.block.BlockNumber,
			Observations:       observations,
			ReplayAttributions: replayAttributions,
			Serialization:      serialization,
			Economics: rustEconomicsObservation{
				SerialServiceNanos: r.serialServiceNanos,
				PreConsensusNanos:  uint64((planningElapsed + preexecutionElapsed).Nanoseconds()),
				PostConsensusNanos: uint64(reconciliationElapsed.Nanoseconds()),
				TransactionCount:   len(txs),
			},
		})
		diag.RustFeedbackNanos = uint64(time.Since(rustFeedbackStarted).Nanoseconds())
		if err != nil {
			return nil, err
		}
		r.lastFeedback = feedback
	} else {
		r.lastFeedback = rustFeedbackResponse{}
	}

	r.last = policyRunStats{
		Attempts:           uint64(len(txs)) + replayCount,
		Reexecutions:       replayCount,
		Speculated:         uint64(len(txs)),
		Reused:             uint64(len(txs)) - replayCount,
		Replayed:           replayCount,
		PreConsensusNanos:  uint64((planningElapsed + preexecutionElapsed).Nanoseconds()),
		PostConsensusNanos: uint64(reconciliationElapsed.Nanoseconds()),
	}
	r.lastDiagnostics = diag
	return results, ctx.Err()
}
