package main

import "testing"

func exactPlanEdges(plan rustPlanResponse) map[[2]int]struct{} {
	out := make(map[[2]int]struct{}, len(plan.Dependencies))
	for _, dep := range plan.Dependencies {
		out[[2]int{dep.Predecessor, dep.Successor}] = struct{}{}
	}
	return out
}

func TestExactTraceACGPlanUsesMinimalRAWVisibilityDependencies(t *testing.T) {
	index := &exactEthereumTraceIndex{byHash: map[string]exactEthereumTraceAccess{
		"0x0": {writes: []string{"evm/a/k"}},
		"0x1": {reads: []string{"evm/a/k"}},
		"0x2": {reads: []string{"evm/a/k"}},
		"0x3": {writes: []string{"evm/a/k"}},
		"0x4": {reads: []string{"evm/a/k"}},
		"0x5": {reads: []string{"evm/a/other"}},
	}}
	block := ExecutionBlock{Transactions: []ExecutionTx{
		{TxHash: "0x0"}, {TxHash: "0x1"}, {TxHash: "0x2"},
		{TxHash: "0x3"}, {TxHash: "0x4"}, {TxHash: "0x5"},
	}}
	plan, diag, err := index.buildExactTraceACGPlan(block)
	if err != nil {
		t.Fatal(err)
	}
	edges := exactPlanEdges(plan)
	for _, want := range [][2]int{{0, 1}, {0, 2}, {3, 4}} {
		if _, ok := edges[want]; !ok {
			t.Fatalf("missing RAW visibility dependency %v in %#v", want, edges)
		}
	}
	for _, unwanted := range [][2]int{{0, 3}, {1, 3}, {2, 3}, {1, 2}} {
		if _, ok := edges[unwanted]; ok {
			t.Fatalf("unnecessary WAW/WAR/read-read dependency %v in %#v", unwanted, edges)
		}
	}
	for pair := range edges {
		if pair[0] == 5 || pair[1] == 5 {
			t.Fatalf("independent resource unexpectedly serialized: %v", pair)
		}
	}
	if diag.MissingTraces != 0 {
		t.Fatalf("missing traces = %d", diag.MissingTraces)
	}
}

func TestExactTraceACGPlanTreatsRevertedWritesAsReads(t *testing.T) {
	index := &exactEthereumTraceIndex{byHash: map[string]exactEthereumTraceAccess{
		"0x0": {writes: []string{"evm/a/k"}},
		"0x1": {writes: []string{"evm/a/k"}},
		"0x2": {reads: []string{"evm/a/k"}},
	}}
	block := ExecutionBlock{Transactions: []ExecutionTx{
		{TxHash: "0x0"},
		{TxHash: "0x1", SourceFailed: true},
		{TxHash: "0x2"},
	}}
	plan, _, err := index.buildExactTraceACGPlan(block)
	if err != nil {
		t.Fatal(err)
	}
	edges := exactPlanEdges(plan)
	if _, ok := edges[[2]int{0, 1}]; !ok {
		t.Fatalf("reverted transaction must still observe the previous writer")
	}
	if _, ok := edges[[2]int{1, 2}]; ok {
		t.Fatalf("discarded reverted write incorrectly ordered a later reader")
	}
}

func TestExactTraceACGPlanMissingTraceIsSerialBarrier(t *testing.T) {
	index := &exactEthereumTraceIndex{byHash: map[string]exactEthereumTraceAccess{
		"0x0": {},
		"0x2": {},
	}}
	block := ExecutionBlock{Transactions: []ExecutionTx{{TxHash: "0x0"}, {TxHash: "0x1"}, {TxHash: "0x2"}}}
	plan, diag, err := index.buildExactTraceACGPlan(block)
	if err != nil {
		t.Fatal(err)
	}
	edges := exactPlanEdges(plan)
	for _, want := range [][2]int{{0, 1}, {1, 2}} {
		if _, ok := edges[want]; !ok {
			t.Fatalf("missing barrier dependency %v", want)
		}
	}
	if diag.MissingTraces != 1 {
		t.Fatalf("missing traces = %d want 1", diag.MissingTraces)
	}
}

func TestExactTraceACGPlanAddsNativeTranslationRAWCompensation(t *testing.T) {
	index := &exactEthereumTraceIndex{byHash: map[string]exactEthereumTraceAccess{
		"0x0": {reads: []string{"evm/a/source-only"}},
		"0x1": {reads: []string{"evm/b/source-only"}},
	}}
	native := &exactNativeTranslationIndex{byBlock: map[uint64]exactNativeTranslationBlock{
		7: {txs: map[int]exactNativeTranslationTx{
			0: {hash: "0x0", writes: []exactNativePoint{{namespace: "storage:contract", key: "shared"}}},
			1: {hash: "0x1", reads: []exactNativePoint{{namespace: "storage:contract", key: "shared"}}},
		}},
	}}
	block := ExecutionBlock{BlockNumber: 7, Transactions: []ExecutionTx{{TxIndex: 0, TxHash: "0x0"}, {TxIndex: 1, TxHash: "0x1"}}}
	plan, diag, err := index.buildExactTraceACGPlanWithTranslation(block, native)
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := exactPlanEdges(plan)[[2]int{0, 1}]; !ok {
		t.Fatalf("missing translation compensation edge")
	}
	if diag.TranslationCompensationEdges != 1 {
		t.Fatalf("translation compensation edges = %d want 1", diag.TranslationCompensationEdges)
	}
}

func TestExactNativeTranslationRangeReadDependsOnPriorWrite(t *testing.T) {
	native := &exactNativeTranslationIndex{byBlock: map[uint64]exactNativeTranslationBlock{
		9: {txs: map[int]exactNativeTranslationTx{
			0: {hash: "0x0", writes: []exactNativePoint{{namespace: "storage:c", key: "m"}}},
			1: {hash: "0x1", ranges: []exactNativeRange{{namespace: "storage:c", start: []byte("a"), end: []byte("z")}}},
		}},
	}}
	block := ExecutionBlock{BlockNumber: 9, Transactions: []ExecutionTx{{TxIndex: 0, TxHash: "0x0"}, {TxIndex: 1, TxHash: "0x1"}}}
	edges := map[[2]int]struct{}{}
	added, err := native.addRAWCompensationEdges(block, edges)
	if err != nil {
		t.Fatal(err)
	}
	if added != 1 {
		t.Fatalf("added = %d want 1", added)
	}
	if _, ok := edges[[2]int{0, 1}]; !ok {
		t.Fatalf("range RAW edge missing")
	}
}
