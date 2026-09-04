package main

import (
	"bytes"
	"os"
	"testing"

	dbm "github.com/cosmos/cosmos-db"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
)

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

func TestSetupDBSnapshotRoundTrip(t *testing.T) {
	db := dbm.NewMemDB()
	defer db.Close()
	want := map[string][]byte{
		"alpha": []byte("one"),
		"beta":  []byte{0, 1, 2, 3},
		"empty": make([]byte, 0),
	}
	for k, v := range want {
		if err := db.Set([]byte(k), v); err != nil {
			t.Fatal(err)
		}
	}
	snapshot, entries, stateBytes, err := captureSetupDB(db)
	if err != nil {
		t.Fatal(err)
	}
	defer os.Remove(snapshot)
	if entries != uint64(len(want)) || stateBytes == 0 {
		t.Fatalf("snapshot entries=%d bytes=%d", entries, stateBytes)
	}
	clone, err := restoreSetupDB(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	defer clone.Close()
	for k, v := range want {
		got, err := clone.Get([]byte(k))
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(got, v) {
			t.Fatalf("key=%s got=%x want=%x", k, got, v)
		}
		if k == "empty" && got == nil {
			t.Fatal("empty database value was restored as nil")
		}
	}
}

func TestCommitIDsEqual(t *testing.T) {
	a := storetypes.CommitID{Version: 7, Hash: []byte{1, 2, 3}}
	b := storetypes.CommitID{Version: 7, Hash: []byte{1, 2, 3}}
	if !commitIDsEqual(a, b) {
		t.Fatal("equal committed states were rejected")
	}
	if commitIDsEqual(a, storetypes.CommitID{Version: 8, Hash: []byte{1, 2, 3}}) {
		t.Fatal("different committed versions were accepted")
	}
	if commitIDsEqual(a, storetypes.CommitID{Version: 7, Hash: []byte{1, 2, 4}}) {
		t.Fatal("different committed hashes were accepted")
	}
	if commitIDsEqual(a, storetypes.CommitID{Version: 7}) {
		t.Fatal("empty committed hash was accepted")
	}
}
