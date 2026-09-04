package main

import (
	"math/rand"
	"os"
	"path/filepath"
	"reflect"
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

func TestVegetaProposalOrderSortsAllDependencyChains(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 5)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}
	// Long chain hot=[0,2,4], then second chain warm=[1,3], then the
	// transaction unique to no additional chain. The old port only moved the
	// single hottest chain and would have returned [0,2,4,1,3] by accident for
	// this simple shape; the Figure-5 test below exercises overlapping chains.
	trackers[0].read(store, []byte("hot"))
	trackers[1].read(store, []byte("warm"))
	trackers[2].write(store, []byte("hot"))
	trackers[3].write(store, []byte("warm"))
	trackers[4].read(store, []byte("hot"))

	got, longest, chains := vegetaProposalOrderWithStats(trackers)
	want := []int{0, 2, 4, 1, 3}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("proposal=%v want=%v", got, want)
	}
	if longest != 3 || chains != 2 {
		t.Fatalf("longest=%d chains=%d want 3,2", longest, chains)
	}
}

func TestVegetaProposalOrderMatchesPaperFigure5(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 6)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}
	// Figure 5 / Algorithm 1 access sets (0-based tx numbering here):
	// tx1 W(a),W(b); tx2 W(d); tx3 W(a); tx4 W(c);
	// tx5 R(a),W(b),R(c); tx6 W(c).
	trackers[0].write(store, []byte("a"))
	trackers[0].write(store, []byte("b"))
	trackers[1].write(store, []byte("d"))
	trackers[2].write(store, []byte("a"))
	trackers[3].write(store, []byte("c"))
	trackers[4].read(store, []byte("a"))
	trackers[4].write(store, []byte("b"))
	trackers[4].read(store, []byte("c"))
	trackers[5].write(store, []byte("c"))

	got, longest, chains := vegetaProposalOrderWithStats(trackers)
	want := []int{0, 2, 4, 3, 5, 1} // tx1,tx3,tx5,tx4,tx6,tx2
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("Figure-5 proposal=%v want=%v", got, want)
	}
	if longest != 3 || chains != 4 {
		t.Fatalf("Figure-5 longest=%d chains=%d want 3,4", longest, chains)
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

	t.Run("same-pair raw plus war promotes to waw", func(t *testing.T) {
		trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
		// Earlier writes a and reads b; later reads a and writes b. There is no
		// ordinary WAW, but Algorithm 1 promotes simultaneous RAW+WAR to WAW.
		trackers[0].write(store, []byte("a"))
		trackers[0].read(store, []byte("b"))
		trackers[1].read(store, []byte("a"))
		trackers[1].write(store, []byte("b"))
		matrix, _ := buildDependencyMatrix([]int{0, 1}, trackers)
		if got := matrix[1][0]; got != dependencyWAW {
			t.Fatalf("RAW+WAR class=%d want promoted WAW=%d", got, dependencyWAW)
		}
		got := nextVegetaBatch(matrix, []bool{false, false})
		if !reflect.DeepEqual(got, []int{0}) {
			t.Fatalf("RAW+WAR promoted batch=%v want [0]", got)
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

func TestVegetaReadyStateMatchesReference(t *testing.T) {
	rng := rand.New(rand.NewSource(0x564547455441))
	classes := []dependencyKinds{0, dependencyWAW, dependencyRAW, dependencyWAR}
	for trial := 0; trial < 10000; trial++ {
		n := 1 + rng.Intn(32)
		matrix := make([][]dependencyKinds, n)
		for later := 0; later < n; later++ {
			matrix[later] = make([]dependencyKinds, n)
			for earlier := 0; earlier < later; earlier++ {
				matrix[later][earlier] = classes[rng.Intn(len(classes))]
			}
		}
		refDone := make([]bool, n)
		stateDone := make([]bool, n)
		state := newVegetaReadyState(matrix)
		completed := 0
		for completed < n {
			want := nextVegetaBatch(matrix, refDone)
			got := state.next(stateDone)
			if len(got) != len(want) {
				t.Fatalf("trial=%d completed=%d batch=%v want=%v", trial, completed, got, want)
			}
			for i := range want {
				if got[i] != want[i] {
					t.Fatalf("trial=%d completed=%d batch=%v want=%v", trial, completed, got, want)
				}
			}
			if len(got) == 0 {
				t.Fatalf("trial=%d DAG stalled completed=%d/%d", trial, completed, n)
			}
			for _, pos := range want {
				refDone[pos] = true
			}
			advanced := state.markDone(got, stateDone)
			if advanced != len(got) {
				t.Fatalf("trial=%d markDone advanced=%d want=%d", trial, advanced, len(got))
			}
			completed += advanced
		}
	}
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

func TestAriaFallbackReleasesSuccessorOnIndividualCompletion(t *testing.T) {
	// Two independent ready transactions start together. tx1 has a short
	// successor chain while tx0 has an unrelated successor. replayAriaP's
	// shrinkDag(done)+popNextTxBatch behavior releases tx3 as soon as tx1
	// finishes; it must not wait for tx0 or tx2 to complete a whole level.
	edges := map[int]map[int]struct{}{
		0: {2: {}},
		1: {3: {}},
		3: {4: {}},
	}
	indegree := map[int]int{0: 0, 1: 0, 2: 1, 3: 1, 4: 1}

	got := ariaReleaseSuccessors(1, indegree, edges)
	if len(got) != 1 || got[0] != 3 {
		t.Fatalf("released after tx1=%v want [3]", got)
	}
	if indegree[2] != 1 {
		t.Fatalf("unrelated tx2 indegree=%d want 1", indegree[2])
	}
	if indegree[3] != 0 {
		t.Fatalf("tx3 indegree=%d want 0", indegree[3])
	}
	if indegree[4] != 1 {
		t.Fatalf("tx4 must still wait for tx3, indegree=%d", indegree[4])
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

func TestAriaFallbackRestoresConflictLostByHotChainReversal(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := make([]*accessTracker, 5)
	for i := range trackers {
		trackers[i] = newAccessTracker(nil)
	}

	// Historical BuildDAG has the conflict path tx0 -> tx1 -> tx2 and removes
	// the direct tx0 -> tx2 edge as transitive. tx2/3/4 form the hot chain.
	// replayAriaP reverses tx1 -> tx2 into tx2 -> tx1; without restoring the
	// omitted tx0/tx2 relation, tx0 and tx2 become unordered even though they
	// still conflict on z.
	trackers[0].write(store, []byte("x"))
	trackers[0].write(store, []byte("z"))
	trackers[1].read(store, []byte("x"))
	trackers[1].write(store, []byte("y"))
	trackers[2].read(store, []byte("y"))
	trackers[2].read(store, []byte("z"))
	trackers[2].read(store, []byte("hot"))
	trackers[3].read(store, []byte("hot"))
	trackers[4].read(store, []byte("hot"))

	edges := ariaFallbackEdges([]int{0, 1, 2, 3, 4}, trackers)
	if _, ok := edges[2][0]; !ok {
		t.Fatalf("fallback edges=%v; hot-chain reversal must retain an ordering for conflicting tx2/tx0", edges)
	}
	if _, ok := edges[2][1]; !ok {
		t.Fatalf("fallback edges=%v; hot-chain tx2 must retain priority over tx1", edges)
	}
}

func TestVegetaAlgorithm3Case1KnownNewKeyDefersFinal(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	// tx1 establishes k in all_keys during speculation. tx0 did not access k
	// during speculation but newly reads it during replay: Algorithm 3 lines 5-8
	// require final serial re-execution.
	pre[1].read(store, []byte("k"))
	actual[0].read(store, []byte("k"))
	actual[1].read(store, []byte("k"))
	matrix, _ := buildDependencyMatrix([]int{0, 1}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := validation.deferred[0]; !ok {
		t.Fatalf("deferred=%v; Algorithm-3 Case 1 must defer tx0", validation.deferred)
	}
	if _, ok := validation.immediate[0]; ok {
		t.Fatalf("immediate=%v; known-key change belongs to final TxsRe", validation.immediate)
	}
}

func TestVegetaAlgorithm3Case2UnknownReadWithoutWriterCommits(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil)}
	actual[0].read(store, []byte("new-key"))
	matrix, _ := buildDependencyMatrix([]int{0}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0},
		map[int]int{0: 0},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if len(validation.deferred) != 0 || len(validation.immediate) != 0 {
		t.Fatalf("deferred=%v immediate=%v; unknown read with no new writer must commit", validation.deferred, validation.immediate)
	}
	if !reflect.DeepEqual(validation.acceptedOrder, []int{0}) {
		t.Fatalf("accepted=%v want [0]", validation.acceptedOrder)
	}
}

func TestVegetaAlgorithm3Case2UnknownReadOfNewWriteReplaysImmediately(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual[0].read(store, []byte("new-key"))
	actual[1].write(store, []byte("new-key"))
	matrix, _ := buildDependencyMatrix([]int{0, 1}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if len(validation.deferred) != 0 {
		t.Fatalf("final deferred=%v; new-key reader should replay immediately, not at block end", validation.deferred)
	}
	if _, ok := validation.immediate[0]; !ok {
		t.Fatalf("immediate=%v; new reader must replay after the batch", validation.immediate)
	}
	if _, ok := validation.immediate[1]; ok {
		t.Fatalf("immediate=%v; write-only new-key transaction should remain accepted", validation.immediate)
	}
	if !reflect.DeepEqual(validation.acceptedOrder, []int{1}) {
		t.Fatalf("accepted=%v want [1]", validation.acceptedOrder)
	}
}

func TestVegetaAlgorithm3Case3UnknownWriteAloneCommits(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil)}
	actual[0].write(store, []byte("new-key"))
	matrix, _ := buildDependencyMatrix([]int{0}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0},
		map[int]int{0: 0},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if len(validation.deferred) != 0 || len(validation.immediate) != 0 {
		t.Fatalf("deferred=%v immediate=%v; Algorithm-3 Case 3 writer must commit without replay", validation.deferred, validation.immediate)
	}
	if !reflect.DeepEqual(validation.acceptedOrder, []int{0}) {
		t.Fatalf("accepted=%v want [0]", validation.acceptedOrder)
	}
}

func TestVegetaWasmdRangeValidationReplaysUnknownReaderOfNewWrite(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual[0].readRange(store, []byte("a"), []byte("z"))
	actual[1].write(store, []byte("m"))
	matrix, _ := buildDependencyMatrix([]int{0, 1}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := validation.immediate[0]; !ok {
		t.Fatalf("immediate=%v; new range reader must replay when another tx newly writes inside its range", validation.immediate)
	}
}

func TestVegetaWasmdRangeValidationDefersRangeCoveringSpeculativePoint(t *testing.T) {
	store := storeIDFromName("wasm")
	pre := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	actual := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil)}
	// Raw speculative point locations are retained by the real Vegeta pre-pass;
	// enable that behavior explicitly in this unit test.
	pre[1].retainReadLocations = true
	pre[1].read(store, []byte("m"))
	actual[0].readRange(store, []byte("a"), []byte("z"))
	actual[1].read(store, []byte("m"))
	matrix, _ := buildDependencyMatrix([]int{0, 1}, pre)
	validation, err := vegetaValidateBatch(
		[]int{0, 1},
		map[int]int{0: 0, 1: 1},
		matrix,
		pre,
		actual,
		buildVegetaUniverse(pre),
	)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := validation.deferred[0]; !ok {
		t.Fatalf("deferred=%v; new range covering a speculative point key must conservatively defer", validation.deferred)
	}
}

func TestAccessTrackersEqualIncludesReadsRangesAndWrites(t *testing.T) {
	store := storeIDFromName("wasm")
	left := newAccessTracker(nil)
	right := newAccessTracker(nil)
	for _, tracker := range []*accessTracker{left, right} {
		tracker.read(store, []byte("r"))
		tracker.readRange(store, []byte("a"), []byte("z"))
		tracker.write(store, []byte("w"))
	}
	if !accessTrackersEqual(left, right) {
		t.Fatal("identical concrete access footprints must compare equal")
	}
	right.read(store, []byte("extra"))
	if accessTrackersEqual(left, right) {
		t.Fatal("new concrete read must invalidate exact oracle footprint equality")
	}
}
