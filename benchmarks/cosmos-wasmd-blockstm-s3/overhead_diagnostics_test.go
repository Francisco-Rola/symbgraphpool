package main

import "testing"

func TestAddSerialOverheadControl(t *testing.T) {
	dst := serialOverheadControl{ActiveNanos: 10, BranchNanos: 2, ExecuteNanos: 7, WriteNanos: 1, SerialEquivalent: true}
	src := serialOverheadControl{ActiveNanos: 20, BranchNanos: 3, ExecuteNanos: 15, WriteNanos: 2, SerialEquivalent: true}
	addSerialOverheadControl(&dst, src)
	if dst.ActiveNanos != 30 || dst.BranchNanos != 5 || dst.ExecuteNanos != 22 || dst.WriteNanos != 3 {
		t.Fatalf("unexpected aggregate: %#v", dst)
	}
	if !dst.SerialEquivalent {
		t.Fatal("serial-equivalent controls should remain true")
	}
}

func TestAddSerialOverheadControlPreservesFailedEquivalence(t *testing.T) {
	dst := serialOverheadControl{SerialEquivalent: true}
	src := serialOverheadControl{ActiveNanos: 1, SerialEquivalent: false}
	addSerialOverheadControl(&dst, src)
	if dst.SerialEquivalent {
		t.Fatal("failed serial equivalence must propagate")
	}
}
