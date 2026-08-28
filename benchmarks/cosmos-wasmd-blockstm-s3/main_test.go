package main

import "testing"

func TestLogicalAddressDeterministic(t *testing.T) {
	a := logicalAddress("0x1111111111111111111111111111111111111111")
	b := logicalAddress("0x1111111111111111111111111111111111111111")
	if !a.Equals(b) || len(a) != 20 {
		t.Fatalf("bad deterministic address")
	}
}
func TestRewriteAddress(t *testing.T) {
	v := map[string]any{"owner": "0xabc", "nested": []any{"native-s3-admin"}}
	r := rewrite(v, map[string]string{"0xabc": "wasm1owner", "native-s3-admin": "wasm1admin"}).(map[string]any)
	if r["owner"] != "wasm1owner" {
		t.Fatal(r)
	}
}
