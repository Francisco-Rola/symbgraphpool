package main

import (
	"os"
	"path/filepath"
	"testing"
)

func rustString(value string) *string { return &value }

func TestCloneBytesPreservesNilAndEmptyValues(t *testing.T) {
	if got := cloneBytes(nil); got != nil {
		t.Fatalf("cloneBytes(nil)=%v want nil", got)
	}
	empty := make([]byte, 0)
	got := cloneBytes(empty)
	if got == nil {
		t.Fatal("cloneBytes(empty) collapsed a valid zero-length value to nil")
	}
	if len(got) != 0 {
		t.Fatalf("len(cloneBytes(empty))=%d want 0", len(got))
	}
}

func TestRustBridgeConfigIgnoresSymbolicBundleManifest(t *testing.T) {
	dir := t.TempDir()
	profile := `{"contract":"cw20-base","profiles":[{"entrypoint":"execute::Transfer","accesses":[]}]}`
	if err := os.WriteFile(filepath.Join(dir, "cw20-base.symbolic.json"), []byte(profile), 0o644); err != nil {
		t.Fatal(err)
	}
	bundleManifest := `{"schema_version":1,"dataset":"vegeta-s4","profiles":[{"native_code_family":"cw20-base","file":"cw20-base.symbolic.json"}]}`
	if err := os.WriteFile(filepath.Join(dir, "manifest.json"), []byte(bundleManifest), 0o644); err != nil {
		t.Fatal(err)
	}
	config, err := loadRustBridgeConfig(".", dir)
	if err != nil {
		t.Fatal(err)
	}
	if got := len(config.Documents); got != 1 {
		t.Fatalf("loaded %d symbolic documents, want 1", got)
	}
	if !config.hasProfile("cw20-base", "execute::Transfer") {
		t.Fatal("contract profile beside bundle manifest was not indexed")
	}
}

func TestRustBridgeConfigLoadsCheckedInNativeProfiles(t *testing.T) {
	repoRoot := filepath.Clean("../..")
	config, err := loadRustBridgeConfig(repoRoot, "benchmarks/symbolic/native-s3")
	if err != nil {
		t.Fatal(err)
	}
	if len(config.Documents) < 10 {
		t.Fatalf("loaded %d symbolic documents, want at least 10", len(config.Documents))
	}
	foundCW20 := false
	for _, document := range config.Documents {
		if document.Family == "cw20-base" {
			foundCW20 = true
			if len(document.Document) == 0 {
				t.Fatal("cw20-base raw symbolic JSON is empty")
			}
		}
	}
	if !foundCW20 {
		t.Fatal("checked-in Rust bridge configuration omitted cw20-base")
	}
}

func TestRustPlanRequestConservesAtomicTransactionCost(t *testing.T) {
	family, instance, sender := "cw20-base", "token-a", "alice"
	block := ExecutionBlock{
		BlockNumber: 7,
		Timestamp:   11,
		Transactions: []ExecutionTx{{
			TxIndex: 0,
			Calls: []CallSpec{
				{
					Kind:       "execute",
					Family:     &family,
					InstanceID: &instance,
					Sender:     &sender,
					Msg:        map[string]any{"transfer": map[string]any{"recipient": "bob", "amount": "1"}},
				},
				{
					Kind:       "execute",
					Family:     &family,
					InstanceID: &instance,
					Sender:     &sender,
					Msg:        map[string]any{"burn": map[string]any{"amount": "1"}},
				},
			},
		}},
	}
	request, err := buildRustPlanRequest(block, []uint32{5}, map[rustSymbolicProfileKey]struct{}{
		{Family: family, Entrypoint: "execute::Transfer"}: {},
		{Family: family, Entrypoint: "execute::Burn"}:     {},
	})
	if err != nil {
		t.Fatal(err)
	}
	components := request.Transactions[0].Components
	if len(components) != 2 {
		t.Fatalf("components=%d want 2", len(components))
	}
	got := components[0].EstimatedExecutionCost + components[1].EstimatedExecutionCost
	if got != 5 {
		t.Fatalf("component costs sum=%d want exact parent cost 5", got)
	}
	if components[0].Entrypoint != "execute::Transfer" || components[1].Entrypoint != "execute::Burn" {
		t.Fatalf("unexpected canonical entrypoints: %#v", components)
	}
}

func TestRustPlanRequestSkipsEntrypointsMissingFromSymbolicCorpus(t *testing.T) {
	repoRoot := filepath.Clean("../..")
	config, err := loadRustBridgeConfig(repoRoot, "benchmarks/symbolic/native-s3")
	if err != nil {
		t.Fatal(err)
	}
	family, instance := "astroport-pair", "pair-a"
	block := ExecutionBlock{BlockNumber: 1, Transactions: []ExecutionTx{{
		TxIndex: 0,
		Calls: []CallSpec{{
			Kind:       "query",
			Family:     &family,
			InstanceID: &instance,
			Msg:        map[string]any{"token1": map[string]any{}},
		}},
	}}}
	request, err := buildRustPlanRequest(block, []uint32{7}, config.Profiles)
	if err != nil {
		t.Fatal(err)
	}
	if got := len(request.Transactions[0].Components); got != 0 {
		t.Fatalf("missing symbolic entrypoint produced %d Rust components, want 0", got)
	}
	if config.hasProfile(family, "query::Token1") {
		t.Fatal("test precondition changed: Token1 is now present in checked-in symbolic corpus")
	}
	if !config.hasProfile(family, "query::GetReserves") {
		t.Fatal("known astroport-pair symbolic profile was not indexed")
	}
}

func TestRustPlanRequestAddsAdapterBankHardResources(t *testing.T) {
	from, to := "alice", "bob"
	block := ExecutionBlock{BlockNumber: 1, Transactions: []ExecutionTx{{
		TxIndex: 0,
		Calls: []CallSpec{{
			Kind:  "bank_send",
			From:  &from,
			To:    &to,
			Coins: []CoinSpec{{Denom: "unative", Amount: "10"}},
		}},
	}}}
	request, err := buildRustPlanRequest(block, nil, map[rustSymbolicProfileKey]struct{}{})
	if err != nil {
		t.Fatal(err)
	}
	got := request.Transactions[0].HardResources
	want := []string{"bank:alice:unative", "bank:bob:unative"}
	if len(got) != len(want) || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("hard resources=%v want %v", got, want)
	}
	if len(request.Transactions[0].Components) != 0 {
		t.Fatal("bank-only transaction must not fabricate a contract symbolic component")
	}
}

func TestRustDependencyTopologyHasNoDiagnosticLevelBarrier(t *testing.T) {
	// 0 -> 2 and 1 -> 3 can be drawn as two diagnostic levels, but transaction 2
	// depends only on 0. Once 0 finishes, unrelated transaction 1 cannot block 2.
	dependencies := []rustPlanDependency{{Predecessor: 0, Successor: 2}, {Predecessor: 1, Successor: 3}}
	_, successors, indegree, err := rustDependencyTopology(4, dependencies)
	if err != nil {
		t.Fatal(err)
	}
	indegree[2]-- // completion of predecessor 0
	if indegree[2] != 0 {
		t.Fatalf("tx 2 remained blocked by an unrelated same-level transaction: indegree=%d", indegree[2])
	}
	if len(successors[0]) != 1 || successors[0][0] != 2 {
		t.Fatalf("successors[0]=%v want [2]", successors[0])
	}
}

func TestRustConcreteConflictKinds(t *testing.T) {
	store := storeIDFromName("wasm")
	left := newAccessTracker(nil)
	right := newAccessTracker(nil)
	left.read(store, []byte("a"))
	left.write(store, []byte("b"))
	right.write(store, []byte("a"))
	right.read(store, []byte("b"))
	right.write(store, []byte("b"))
	got := rustConflictKinds(left, right)
	want := conflictReadWrite | conflictWriteRead | conflictWriteWrite
	if got != want {
		t.Fatalf("conflict bits=%03b want %03b", got, want)
	}
}

func TestReplayAttributionConservesMeasuredReplayCost(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil), newAccessTracker(nil)}
	trackers[0].write(store, []byte("x"))
	trackers[1].write(store, []byte("y"))
	trackers[2].read(store, []byte("x"))
	trackers[2].read(store, []byte("y"))
	causes := [][]int{nil, nil, {0, 1}}
	items := buildReplayAttributions(trackers, causes, []uint64{0, 0, 7})
	if len(items) != 2 {
		t.Fatalf("attributions=%d want 2", len(items))
	}
	var total uint64
	for _, item := range items {
		total += item.ReplayCostNanos
	}
	if total != 7 {
		t.Fatalf("attributed replay cost=%d want 7", total)
	}
}

func TestRustVisibilityMaskFreezesLaunchState(t *testing.T) {
	completed := make([]uint64, 1)
	markRustCompleted(completed, 0)
	mask := rustVisibilityFromCompleted(completed, 3)
	markRustCompleted(completed, 1)
	if !mask.contains(0) {
		t.Fatal("launch mask lost predecessor completed before launch")
	}
	if mask.contains(1) {
		t.Fatal("launch mask leaked predecessor completed after launch")
	}
	if mask.contains(3) {
		t.Fatal("launch mask leaked current/future canonical transaction")
	}
}

func TestRustBlockMVCCSelectsNewestLaunchVisibleVersion(t *testing.T) {
	storeKey := testStoreKey("wasm")
	mvcc := newRustBlockMVCC()
	mvcc.publish(0, rustTxDelta{entries: []rustTxDeltaEntry{{storeKey: storeKey, key: []byte("k"), bytes: []byte("v0")}}})
	mvcc.publish(1, rustTxDelta{entries: []rustTxDeltaEntry{{storeKey: storeKey, key: []byte("k"), bytes: []byte("v1")}}})
	completed := []uint64{1 << 0}
	mask := rustVisibilityFromCompleted(completed, 2)
	value, deleted, ok := mvcc.bytesValue(storeKey, []byte("k"), 2, mask)
	if !ok || deleted || string(value) != "v0" {
		t.Fatalf("visible value=(%q deleted=%v ok=%v) want v0", value, deleted, ok)
	}
	markRustCompleted(completed, 1)
	// The previously frozen mask must still ignore tx1.
	value, _, _ = mvcc.bytesValue(storeKey, []byte("k"), 2, mask)
	if string(value) != "v0" {
		t.Fatalf("frozen mask changed after later completion: %q", value)
	}
	newMask := rustVisibilityFromCompleted(completed, 2)
	value, deleted, ok = mvcc.bytesValue(storeKey, []byte("k"), 2, newMask)
	if !ok || deleted || string(value) != "v1" {
		t.Fatalf("new launch did not select newest visible value: %q deleted=%v ok=%v", value, deleted, ok)
	}
}

func TestRustBlockMVCCPreservesDeletionAndEmptyValue(t *testing.T) {
	storeKey := testStoreKey("wasm")
	mvcc := newRustBlockMVCC()
	mvcc.publish(0, rustTxDelta{entries: []rustTxDeltaEntry{{storeKey: storeKey, key: []byte("empty"), bytes: make([]byte, 0)}}})
	mvcc.publish(1, rustTxDelta{entries: []rustTxDeltaEntry{{storeKey: storeKey, key: []byte("gone"), deleted: true}}})
	mask := rustVisibilityFromCompleted([]uint64{(1 << 0) | (1 << 1)}, 2)
	value, deleted, ok := mvcc.bytesValue(storeKey, []byte("empty"), 2, mask)
	if !ok || deleted || value == nil || len(value) != 0 {
		t.Fatalf("empty value=(%v deleted=%v ok=%v) want non-nil empty", value, deleted, ok)
	}
	_, deleted, ok = mvcc.bytesValue(storeKey, []byte("gone"), 2, mask)
	if !ok || !deleted {
		t.Fatalf("deletion=(deleted=%v ok=%v) want visible tombstone", deleted, ok)
	}
}

func TestRustIndexedValidationFindsOnlyInvisibleOrReplayedLatestWriter(t *testing.T) {
	store := storeIDFromName("wasm")
	index := newRustCanonicalWriteIndex()
	writes0 := newWriteSet(1)
	writes0.add(store, []byte("x"))
	index.add(0, writes0)
	writes1 := newWriteSet(1)
	writes1.add(store, []byte("y"))
	index.add(1, writes1)
	reader := newAccessTracker(nil)
	reader.read(store, []byte("x"))
	reader.readRange(store, []byte("y"), []byte("z"))

	visible := rustVisibilityFromCompleted([]uint64{1 << 0}, 2)
	causes := index.invalidating(2, reader, visible, []bool{false, false, false})
	if len(causes) != 1 || causes[0] != 1 {
		t.Fatalf("indexed causes=%v want [1]", causes)
	}
	visible = rustVisibilityFromCompleted([]uint64{(1 << 0) | (1 << 1)}, 2)
	causes = index.invalidating(2, reader, visible, []bool{true, false, false})
	if len(causes) != 1 || causes[0] != 0 {
		t.Fatalf("replayed visible writer causes=%v want [0]", causes)
	}
}

func TestRustCriticalPathUsesDependencyCosts(t *testing.T) {
	deps := []rustPlanDependency{{Predecessor: 0, Successor: 2}, {Predecessor: 1, Successor: 2}, {Predecessor: 2, Successor: 3}}
	length, cost := rustCriticalPath(4, deps, []uint32{5, 7, 11, 13})
	if length != 3 || cost != 31 { // 1 -> 2 -> 3 = 7 + 11 + 13
		t.Fatalf("critical path=(len=%d cost=%d) want (3,31)", length, cost)
	}
}

func TestRustProfileFeedbackOnlyEvaluatesRequestedPairs(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil), newAccessTracker(nil)}
	trackers[0].write(store, []byte("x"))
	trackers[1].read(store, []byte("x"))
	trackers[2].read(store, []byte("x"))
	observations := rustPairObservationsForPlanPairs(trackers, []rustPlanPair{{Left: 0, Right: 2}}, "pre_execution", func(_, _ int) bool { return true })
	if len(observations) != 1 || observations[0].Left != 0 || observations[0].Right != 2 || !observations[0].Conflict {
		t.Fatalf("profile observations=%#v want only conflicting pair 0->2", observations)
	}
}

func TestRustPlanningOverridesParsePolicySweepValues(t *testing.T) {
	overrides, err := rustPlanningOverridesFromStrings("", "0.30", "0.90", "0.50", "0.10", "0.90", "0.20", "8", "4", "0.15")
	if err != nil {
		t.Fatal(err)
	}
	if overrides == nil || overrides.SoftThreshold == nil || *overrides.SoftThreshold != 0.30 || overrides.RiskBudget == nil || *overrides.RiskBudget != 0.50 {
		t.Fatalf("unexpected overrides: %#v", overrides)
	}
	if overrides.IndependentObservationsBeforeSoftening == nil || *overrides.IndependentObservationsBeforeSoftening != 4 {
		t.Fatalf("softening override=%v want 4", overrides.IndependentObservationsBeforeSoftening)
	}
	defaults, err := rustPlanningOverridesFromStrings("", "", "", "", "", "", "", "", "", "")
	if err != nil || defaults != nil {
		t.Fatalf("blank overrides=(%#v,%v) want (nil,nil)", defaults, err)
	}
}

func TestRustCriticalPathReasonDiagnosticsIdentifySemanticBottleneck(t *testing.T) {
	plan := rustPlanResponse{
		Dependencies: []rustPlanDependency{{Predecessor: 0, Successor: 1, Class: "hard"}, {Predecessor: 1, Successor: 2, Class: "soft"}},
		DependencyReasons: []rustPlanDependencyReasons{
			{Predecessor: 0, Successor: 1, Reasons: []string{"symbolic_hard", "projection_hard"}},
			{Predecessor: 1, Successor: 2, Reasons: []string{"soft_risk"}},
		},
	}
	length, cost, path := rustCriticalPathDetail(3, plan.Dependencies, []uint32{5, 7, 11})
	if length != 3 || cost != 23 || len(path) != 3 || path[0] != 0 || path[1] != 1 || path[2] != 2 {
		t.Fatalf("critical path=(len=%d cost=%d path=%v) want (3,23,[0 1 2])", length, cost, path)
	}
	all, primary, critical, criticalCost := rustDependencyReasonDiagnostics(plan, path, []uint32{5, 7, 11})
	if all["projection_hard"] != 1 || primary["symbolic_hard"] != 1 || primary["soft_risk"] != 1 {
		t.Fatalf("dependency reasons all=%v primary=%v", all, primary)
	}
	if critical["symbolic_hard"] != 1 || critical["soft_risk"] != 1 {
		t.Fatalf("critical reasons=%v", critical)
	}
	if criticalCost["root"] != 5 || criticalCost["symbolic_hard"] != 7 || criticalCost["soft_risk"] != 11 {
		t.Fatalf("critical cost attribution=%v want root=5 symbolic=7 soft=11", criticalCost)
	}
}

func TestRustActualConflictOracleExposesHeadroom(t *testing.T) {
	store := storeIDFromName("wasm")
	trackers := []*accessTracker{newAccessTracker(nil), newAccessTracker(nil), newAccessTracker(nil)}
	trackers[0].write(store, []byte("x"))
	trackers[1].read(store, []byte("x"))
	trackers[2].write(store, []byte("z"))
	edges, cpTx, cpCost, parallelism, path := rustActualConflictOracle(trackers, []uint32{5, 7, 11})
	if edges != 1 || cpTx != 2 || cpCost != 12 {
		t.Fatalf("oracle=(edges=%d cpTx=%d cpCost=%d path=%v) want (1,2,12,[0 1])", edges, cpTx, cpCost, path)
	}
	if parallelism <= 1.9 || parallelism >= 2.0 {
		t.Fatalf("oracle parallelism=%f want 23/12", parallelism)
	}
}

func TestRustDependencyDiagnosticsSeparateProvenanceAndDecision(t *testing.T) {
	plan := rustPlanResponse{
		Dependencies: []rustPlanDependency{{Predecessor: 0, Successor: 1, Class: "soft"}},
		DependencyReasons: []rustPlanDependencyReasons{{
			Predecessor: 0,
			Successor:   1,
			Reasons:     []string{"soft_risk"},
			Provenance:  []string{"static_profile"},
			Decisions:   []string{"soft_serialized"},
		}},
	}
	allProv, cpProv := rustDependencyDimensionDiagnostics(plan, []int{0, 1}, "provenance")
	allDecision, cpDecision := rustDependencyDimensionDiagnostics(plan, []int{0, 1}, "decision")
	if allProv["static_profile"] != 1 || cpProv["static_profile"] != 1 {
		t.Fatalf("provenance all=%v cp=%v", allProv, cpProv)
	}
	if allDecision["soft_serialized"] != 1 || cpDecision["soft_serialized"] != 1 {
		t.Fatalf("decision all=%v cp=%v", allDecision, cpDecision)
	}
}
