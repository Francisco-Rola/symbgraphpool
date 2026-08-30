package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestKeyInRange(t *testing.T) {
	cases := []struct {
		name       string
		key, start string
		end        string
		want       bool
	}{
		{name: "inside", key: "b", start: "a", end: "c", want: true},
		{name: "start inclusive", key: "a", start: "a", end: "c", want: true},
		{name: "end exclusive", key: "c", start: "a", end: "c", want: false},
		{name: "unbounded", key: "z", want: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var start, end []byte
			if tc.start != "" {
				start = []byte(tc.start)
			}
			if tc.end != "" {
				end = []byte(tc.end)
			}
			if got := keyInRange([]byte(tc.key), start, end); got != tc.want {
				t.Fatalf("keyInRange(%q,%q,%q)=%v want %v", tc.key, tc.start, tc.end, got, tc.want)
			}
		})
	}
}

func TestReadsConflictWithWritesExactAndRange(t *testing.T) {
	reads := newAccessTracker(nil)
	key := testStoreKey("wasm")
	store := storeIDFromName(key.Name())
	reads.read(store, []byte("exact"))
	reads.readRange(store, []byte("p/"), []byte("q/"))

	exactWrites := newWriteSet(1)
	exactWrites.add(store, []byte("exact"))
	if !readsConflictWithWrites(reads, &exactWrites) {
		t.Fatal("expected exact read/write conflict")
	}
	rangeWrites := newWriteSet(1)
	rangeWrites.add(store, []byte("p/123"))
	if !readsConflictWithWrites(reads, &rangeWrites) {
		t.Fatal("expected iterator-range conflict")
	}
	outsideWrites := newWriteSet(1)
	outsideWrites.add(store, []byte("z"))
	if readsConflictWithWrites(reads, &outsideWrites) {
		t.Fatal("unexpected conflict outside read set/range")
	}
}

func TestNestedTrackerReadBubblesWithoutWrite(t *testing.T) {
	parent := newAccessTracker(nil)
	child := newAccessTracker(parent)
	key := testStoreKey("wasm")
	store := storeIDFromName(key.Name())
	child.read(store, []byte("observed-before-revert"))
	writes := newWriteSet(1)
	writes.add(store, []byte("observed-before-revert"))
	if !readsConflictWithWrites(parent, &writes) {
		t.Fatal("discarded nested-cache read must remain part of the transaction validation set")
	}
}

type testStoreKey string

func (k testStoreKey) Name() string   { return string(k) }
func (k testStoreKey) String() string { return string(k) }

func ptr(s string) *string { return &s }

func access(scope, resource, key string, write bool) predictedAccess {
	return predictedAccess{location: predictedLocation{scope: scope, resource: resource, key: key}, write: write}
}

func footprint(accesses ...predictedAccess) staticFootprint {
	return staticFootprint{accesses: accesses}
}

func TestStaticDependsMatchesRustPredictedConflict(t *testing.T) {
	read := footprint(access("pair", "RESERVES", "singleton", false))
	write := footprint(access("pair", "RESERVES", "singleton", true))
	other := footprint(access("other", "RESERVES", "singleton", true))
	wildcard := footprint(access("pair", "RESERVES", "*", true))

	if !staticDepends(read, write) {
		t.Fatal("native Rust predicted_conflict includes earlier-read/later-write")
	}
	if !staticDepends(write, read) {
		t.Fatal("native Rust predicted_conflict includes earlier-write/later-read")
	}
	if staticDepends(read, read) {
		t.Fatal("read/read should not conflict")
	}
	if staticDepends(read, other) {
		t.Fatal("different symbolic scopes should not conflict")
	}
	if !staticDepends(read, wildcard) {
		t.Fatal("wildcard key must overlap concrete key")
	}
}

func TestBuildStaticLevelsFromSymbolicPredictions(t *testing.T) {
	block := ExecutionBlock{BlockNumber: 99, Transactions: []ExecutionTx{{}, {}, {}}}
	accesses := symbolicAccessIndex{
		{block: 99, tx: 0}: footprint(access("pair", "A", "singleton", true)),
		{block: 99, tx: 1}: footprint(access("pair", "B", "singleton", true)),
		{block: 99, tx: 2}: footprint(access("pair", "A", "singleton", false)),
	}
	levels, err := buildStaticLevels(block, accesses)
	if err != nil {
		t.Fatal(err)
	}
	if len(levels) != 2 {
		t.Fatalf("got %d levels, want 2: %#v", len(levels), levels)
	}
	if len(levels[0]) != 2 || levels[0][0] != 0 || levels[0][1] != 1 {
		t.Fatalf("level 0=%v, want [0 1]", levels[0])
	}
	if len(levels[1]) != 1 || levels[1][0] != 2 {
		t.Fatalf("level 1=%v, want [2]", levels[1])
	}
}

func TestNormalizeEntrypointMatchesRust(t *testing.T) {
	if got, want := normalizeEntrypoint("execute::TransferFrom"), "executetransferfrom"; got != want {
		t.Fatalf("normalizeEntrypoint=%q want %q", got, want)
	}
}

func TestResolveSymbolicKeyMatchesRustInputRules(t *testing.T) {
	call := CallSpec{
		Kind:   "execute",
		Sender: ptr("sender"),
		Msg: map[string]any{"transfer_from": map[string]any{
			"owner":   "alice",
			"spender": "carol",
		}},
	}
	ownerSpender := symbolicKey{DependsOn: &symbolicDependency{OriginInput: ptr("(owner, spender)")}}
	if got, want := resolveSymbolicKey(call, ownerSpender), "alice|carol"; got != want {
		t.Fatalf("tuple key=%q want %q", got, want)
	}
	senderKey := symbolicKey{DependsOn: &symbolicDependency{OriginInput: ptr("info.sender")}}
	if got, want := resolveSymbolicKey(call, senderKey), "sender"; got != want {
		t.Fatalf("sender key=%q want %q", got, want)
	}
	unknown := symbolicKey{SemanticName: ptr("address"), DependsOn: &symbolicDependency{OriginInput: ptr("missing")}}
	if got := resolveSymbolicKey(call, unknown); got != "*" {
		t.Fatalf("unresolved input must become wildcard, got %q", got)
	}
}

func TestSymbolicPredictorSyntheticProfileAndBankDependencies(t *testing.T) {
	dir := t.TempDir()
	profile := `{
  "contract":"cw20-base",
  "profiles":[{
    "entrypoint":"execute::Transfer",
    "accesses":[
      {"kind":"read","resource":"BALANCES","key":{"semantic_name":"address","depends_on":{"origin_input":"info.sender"}}},
      {"kind":"write","resource":"BALANCES","key":{"semantic_name":"address","depends_on":{"origin_input":"recipient"}}}
    ]
  }]
}`
	if err := os.WriteFile(filepath.Join(dir, "cw20-base.symbolic.json"), []byte(profile), 0o644); err != nil {
		t.Fatal(err)
	}
	predictor, err := loadSymbolicPredictor(".", dir)
	if err != nil {
		t.Fatal(err)
	}
	family, instance, sender := "cw20-base", "token-A", "alice"
	tx := ExecutionTx{Calls: []CallSpec{{
		Kind:       "execute",
		Family:     &family,
		InstanceID: &instance,
		Sender:     &sender,
		Msg:        map[string]any{"transfer": map[string]any{"recipient": "bob", "amount": "10"}},
		Funds:      []CoinSpec{{Denom: "unative", Amount: "1"}},
	}}}
	fp := predictor.predictTx(tx)
	want := []predictedAccess{
		access("bank", "alice", "unative", true),
		access("bank", "token-A", "unative", true),
		access("token-A", "BALANCES", "alice", false),
		access("token-A", "BALANCES", "bob", true),
	}
	if len(fp.accesses) != len(want) {
		t.Fatalf("access count=%d want %d: %#v", len(fp.accesses), len(want), fp.accesses)
	}
	for i := range want {
		if fp.accesses[i] != want[i] {
			t.Fatalf("access[%d]=%#v want %#v", i, fp.accesses[i], want[i])
		}
	}
}

func TestNativeS3SymbolicProfilesLoadAndPredict(t *testing.T) {
	repoRoot := filepath.Clean("../..")
	predictor, err := loadSymbolicPredictor(repoRoot, "benchmarks/symbolic/native-s3")
	if err != nil {
		t.Fatal(err)
	}
	if predictor.documentCount < 10 {
		t.Fatalf("loaded %d symbolic documents, want at least 10", predictor.documentCount)
	}
	if predictor.profileCount < 80 {
		t.Fatalf("loaded %d profiles, want at least 80", predictor.profileCount)
	}
	family, instance, sender := "astroport-pair", "astroport-pair:0xabc", "trader"
	tx := ExecutionTx{Calls: []CallSpec{{
		Kind:       "execute",
		Family:     &family,
		InstanceID: &instance,
		Sender:     &sender,
		Msg: map[string]any{"swap": map[string]any{
			"offer_index": float64(0), "amount_in": "100", "min_out": "1", "recipient": "trader",
		}},
	}}}
	fp := predictor.predictTx(tx)
	foundRead, foundWrite := false, false
	for _, a := range fp.accesses {
		if a.location.scope == instance && a.location.resource == "RESERVES" && a.location.key == "singleton" {
			if a.write {
				foundWrite = true
			} else {
				foundRead = true
			}
		}
	}
	if !foundRead || !foundWrite {
		t.Fatalf("astroport Swap prediction missing RESERVES read/write: %#v", fp.accesses)
	}
}

func TestNestedTrackerWritesCommitOnlyOnMerge(t *testing.T) {
	parent := newAccessTracker(nil)
	child := newAccessTracker(parent)
	key := testStoreKey("wasm")
	store := storeIDFromName(key.Name())
	id := exactAccessID(store, []byte("discard-or-commit"))
	child.write(store, []byte("discard-or-commit"))
	if _, ok := parent.writes.exact[id]; ok {
		t.Fatal("nested write must not bubble before child cache commits")
	}
	child.mergeIntoParent()
	if _, ok := parent.writes.exact[id]; !ok {
		t.Fatal("nested write must bubble when child cache commits")
	}
}

func TestExactAccessFingerprintDeterministicAndAllocFree(t *testing.T) {
	store := storeIDFromName("wasm")
	key := []byte("contract/storage/key/123456789")
	want := exactAccessID(store, key)
	if got := exactAccessID(store, key); got != want {
		t.Fatalf("fingerprint changed: got=%d want=%d", got, want)
	}
	allocs := testing.AllocsPerRun(1000, func() {
		_ = exactAccessID(store, key)
	})
	if allocs != 0 {
		t.Fatalf("exactAccessID allocations/run=%f want 0", allocs)
	}
}

func TestWriteSetDeduplicatesRepeatedWritesAndPreservesRangeKey(t *testing.T) {
	store := storeIDFromName("wasm")
	writes := newWriteSet(2)
	writes.add(store, []byte("p/123"))
	writes.add(store, []byte("p/123"))
	if len(writes.exact) != 1 {
		t.Fatalf("unique writes=%d want 1", len(writes.exact))
	}
	reads := newAccessTracker(nil)
	reads.readRange(store, []byte("p/"), []byte("q/"))
	if !readsConflictWithWrites(reads, &writes) {
		t.Fatal("raw write key must remain available for range validation")
	}
}

func TestPolicyReuseCountersAreConsistent(t *testing.T) {
	stats := policyRunStats{Speculated: 100, Reused: 96, Replayed: 4, Attempts: 104, Reexecutions: 4}
	if stats.Reused+stats.Replayed != stats.Speculated {
		t.Fatalf("reuse+replay=%d want speculated=%d", stats.Reused+stats.Replayed, stats.Speculated)
	}
	if stats.Replayed != stats.Reexecutions {
		t.Fatalf("replayed=%d reexecutions=%d", stats.Replayed, stats.Reexecutions)
	}
}

func TestAriaRule2ForwardFallbacksMatchesRustHarnessConditions(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 4)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}

	// tx0 writes k0. tx1 also writes k0 -> WAW, so tx1 must fallback.
	trackers[0].write(store, []byte("k0"))
	trackers[1].write(store, []byte("k0"))

	// tx2 only reads k0 -> RAW from tx0, but no WAR/WAW, so Rule 2 does
	// not fallback tx2. Its snapshot result serializes before tx0's write.
	trackers[2].read(store, []byte("k0"))

	// tx3 reads k0 (RAW) and writes a key tx0 read (WAR), so it has both
	// dependency directions and must fallback.
	trackers[0].read(store, []byte("k1"))
	trackers[3].read(store, []byte("k0"))
	trackers[3].write(store, []byte("k1"))

	fallbacks, discovered := ariaRule2ForwardFallbacks(trackers)
	if discovered == 0 {
		t.Fatal("expected discovered Aria conflict pairs")
	}
	if _, ok := fallbacks[1]; !ok {
		t.Fatal("WAW transaction must be a Rule-2 fallback")
	}
	if _, ok := fallbacks[2]; ok {
		t.Fatal("RAW-only transaction should not be a proactive Rule-2 fallback")
	}
	if _, ok := fallbacks[3]; !ok {
		t.Fatal("combined WAR+RAW transaction must be a Rule-2 fallback")
	}
}

func TestVegetaProposalOrderMovesHottestChainFirst(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 4)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}
	trackers[0].read(store, []byte("hot"))
	trackers[1].read(store, []byte("cold"))
	trackers[2].write(store, []byte("hot"))
	trackers[3].read(store, []byte("hot"))

	got := vegetaProposalOrder(trackers)
	want := []int{0, 2, 3, 1}
	if len(got) != len(want) {
		t.Fatalf("proposal=%v want=%v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("proposal=%v want=%v", got, want)
		}
	}
}

func TestNextVegetaBatchMatchesUpstreamRule2(t *testing.T) {
	store := storeIDFromName("wasm")

	t.Run("raw only is ready", func(t *testing.T) {
		trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
		trackers[0].write(store, []byte("k"))
		trackers[1].read(store, []byte("k"))
		matrix, _ := buildDependencyMatrix([]int{0, 1}, trackers)
		got := nextVegetaBatch(matrix, []bool{false, false})
		if len(got) != 2 || got[0] != 0 || got[1] != 1 {
			t.Fatalf("RAW-only batch=%v want [0 1]", got)
		}
	})

	t.Run("waw blocks later", func(t *testing.T) {
		trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
		trackers[0].write(store, []byte("k"))
		trackers[1].write(store, []byte("k"))
		matrix, _ := buildDependencyMatrix([]int{0, 1}, trackers)
		got := nextVegetaBatch(matrix, []bool{false, false})
		if len(got) != 1 || got[0] != 0 {
			t.Fatalf("WAW batch=%v want [0]", got)
		}
	})

	t.Run("raw from one predecessor plus war from another blocks later", func(t *testing.T) {
		trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil), newAccessTracker(nil)}
		// tx2 has RAW from tx0 and WAR against tx1. Upstream stores one
		// dependency class per pair, so the two directions must come from
		// distinct predecessor relationships to make Rule 2 block tx2.
		trackers[0].write(store, []byte("a"))
		trackers[1].read(store, []byte("b"))
		trackers[2].read(store, []byte("a"))
		trackers[2].write(store, []byte("b"))
		matrix, _ := buildDependencyMatrix([]int{0, 1, 2}, trackers)
		got := nextVegetaBatch(matrix, []bool{false, false, false})
		if len(got) != 2 || got[0] != 0 || got[1] != 1 {
			t.Fatalf("RAW+WAR batch=%v want [0 1]", got)
		}
	})

	t.Run("waw precedence matches upstream", func(t *testing.T) {
		trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
		trackers[0].write(store, []byte("a"))
		trackers[0].read(store, []byte("b"))
		trackers[1].write(store, []byte("a"))
		trackers[1].read(store, []byte("a"))
		trackers[1].write(store, []byte("b"))
		matrix, _ := buildDependencyMatrix([]int{0, 1}, trackers)
		if got := matrix[1][0]; got != dependencyWAW {
			t.Fatalf("dependency class=%d want WAW precedence=%d", got, dependencyWAW)
		}
	})
}

func TestSnapshotSerializationReversesRawOnly(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	trackers[0].write(store, []byte("k"))
	trackers[1].read(store, []byte("k"))
	order, err := serializationOrderForSnapshot(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		trackers,
	)
	if err != nil {
		t.Fatal(err)
	}
	if len(order) != 2 || order[0] != 1 || order[1] != 0 {
		t.Fatalf("RAW-only serialization=%v want [1 0]", order)
	}
}

func TestAriaDirectPredecessorsRemoveTransitiveConflictEdge(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil), newAccessTracker(nil)}
	// All three touch the same key, so conflicts exist for 0-1, 1-2, and
	// 0-2. Upstream BuildDAG keeps 1->2 and 0->1; 0->2 is transitive.
	trackers[0].write(store, []byte("k"))
	trackers[1].write(store, []byte("k"))
	trackers[2].write(store, []byte("k"))
	preds := ariaDirectPredecessors([]int{0, 1, 2}, trackers)
	if _, ok := preds[1][0]; !ok {
		t.Fatalf("preds=%v; tx1 must directly depend on tx0", preds)
	}
	if _, ok := preds[2][1]; !ok {
		t.Fatalf("preds=%v; tx2 must directly depend on tx1", preds)
	}
	if _, ok := preds[2][0]; ok {
		t.Fatalf("preds=%v; transitive tx0->tx2 edge must be removed", preds)
	}
}

func TestAriaFallbackHotChainPrecedesEarlierConflictingNonChain(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 4)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}
	// x conflicts only between tx0 and tx1. hot is touched by tx1/2/3, so the
	// hot chain is [1 2 3]. replayAriaP reverses tx0->tx1 into tx1->tx0.
	trackers[0].write(store, []byte("x"))
	trackers[1].read(store, []byte("x"))
	trackers[1].read(store, []byte("hot"))
	trackers[2].read(store, []byte("hot"))
	trackers[3].read(store, []byte("hot"))
	edges := ariaFallbackEdges([]int{0, 1, 2, 3}, trackers)
	if _, ok := edges[1][0]; !ok {
		t.Fatalf("fallback edges=%v; hot-chain tx1 must precede conflicting earlier tx0", edges)
	}
}

func TestVegetaAccessChangeAllowsUnknownReadWithoutWriter(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := newAccessTracker(nil)
	actual := newAccessTracker(nil)
	actual.read(store, []byte("new-key"))
	change := classifyVegetaAccessChange(pre, actual, buildVegetaUniverse([]*accessTracker{pre}))
	if change.changed {
		t.Fatal("previously unseen read with no speculative writer should not force re-execution")
	}
	if len(change.newReadIDs) != 1 {
		t.Fatalf("new reads=%d want 1", len(change.newReadIDs))
	}
}

func TestVegetaBatchDefersUnknownReadOfConcurrentUnknownWrite(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual[0].read(store, []byte("new-key"))
	actual[1].write(store, []byte("new-key"))
	deferred, immediate, accepted, err := vegetaValidateBatch(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if len(deferred) != 0 {
		t.Fatalf("final deferred=%v; new-key reader should replay immediately, not at block end", deferred)
	}
	if _, ok := immediate[0]; !ok {
		t.Fatalf("immediate=%v; new reader must replay after the batch", immediate)
	}
	if _, ok := immediate[1]; ok {
		t.Fatalf("immediate=%v; write-only new-key transaction should remain accepted", immediate)
	}
	if len(accepted) != 1 || accepted[0] != 1 {
		t.Fatalf("accepted=%v want [1]", accepted)
	}
}
