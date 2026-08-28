package main

import "testing"

func TestScopedKeyUnifiesReadWriteKinds(t *testing.T) {
	raw := []byte{1, 2, 3}
	r := scopedKey(Access{Kind: "storage_read", Contract: "c"}, raw)
	w := scopedKey(Access{Kind: "storage_write", Contract: "c"}, raw)
	if string(r) != string(w) {
		t.Fatalf("storage read/write must share key")
	}
	b := scopedKey(Access{Kind: "bank_write", Contract: "c"}, raw)
	if string(r) == string(b) {
		t.Fatalf("bank/storage namespaces must remain distinct")
	}
}

func TestDeterministicCompute(t *testing.T) {
	if deterministicCompute(1000) != deterministicCompute(1000) {
		t.Fatal("compute must be deterministic")
	}
}
