//go:build acg_rust

package main

import (
	"path/filepath"
	"testing"
)

func TestRustCGOBridgePlansRealCheckedInCW20ProfileAndAcceptsFeedback(t *testing.T) {
	repoRoot := filepath.Clean("../..")
	bridge, err := NewRustSymbGraphBridge(repoRoot, "benchmarks/symbolic/native-s3")
	if err != nil {
		t.Fatal(err)
	}
	defer bridge.Close()

	family, instance, sender := "cw20-base", "token-a", "alice"
	transfer := func(recipient string) CallSpec {
		return CallSpec{
			Kind:       "execute",
			Family:     &family,
			InstanceID: &instance,
			Sender:     &sender,
			Msg:        map[string]any{"transfer": map[string]any{"recipient": recipient, "amount": "1"}},
		}
	}
	block := ExecutionBlock{
		BlockNumber: 1,
		Timestamp:   1,
		Transactions: []ExecutionTx{
			{TxIndex: 0, Calls: []CallSpec{transfer("bob")}},
			{TxIndex: 1, Calls: []CallSpec{transfer("carol")}},
		},
	}
	plan, err := bridge.Plan(block, []uint32{10, 10})
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, dependency := range plan.Dependencies {
		if dependency.Predecessor == 0 && dependency.Successor == 1 {
			found = true
		}
	}
	if !found {
		t.Fatalf("real Rust ACG plan did not order conflicting same-instance transfers: %#v", plan.Dependencies)
	}

	feedback, err := bridge.Feedback(rustFeedbackRequest{
		Epoch: 1,
		Observations: []rustPairObservation{{
			Left:          0,
			Right:         1,
			ConflictKinds: conflictWriteWrite,
			Conflict:      true,
			Source:        "pre_execution",
		}},
		Serialization: []rustSerializationObservation{{
			Predecessor:             0,
			Transaction:             1,
			MarginalReadyDelayNanos: 17,
		}},
		Economics: rustEconomicsObservation{SerialServiceNanos: 100, PreConsensusNanos: 50, PostConsensusNanos: 25, TransactionCount: 2},
	})
	if err != nil {
		t.Fatal(err)
	}
	if feedback.AppliedObservations == 0 {
		t.Fatal("concrete pre-execution observation did not reach acg-feedback")
	}
	if feedback.SerializationObservations == 0 {
		t.Fatal("serialization-cost evidence did not reach the Rust adaptive store")
	}
}
