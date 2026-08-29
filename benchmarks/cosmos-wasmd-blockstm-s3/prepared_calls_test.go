package main

import (
	"bytes"
	"testing"

	sdk "github.com/cosmos/cosmos-sdk/types"
)

func strPtr(s string) *string { return &s }

func TestPrepareWorkloadCallsMaterializesStaticPayloads(t *testing.T) {
	contract := logicalAddress("prepared-contract")
	sender := logicalAddress("prepared-sender")
	to := logicalAddress("prepared-to")
	bench := &benchApp{
		contracts: map[string]sdk.AccAddress{"pair": contract},
		addresses: map[string]sdk.AccAddress{"alice": sender, "bob": to},
		repl: map[string]string{
			"pair":  contract.String(),
			"alice": sender.String(),
			"bob":   to.String(),
		},
	}
	blocks := []ExecutionBlock{{
		BlockNumber: 7,
		Transactions: []ExecutionTx{{
			TxIndex: 3,
			Calls: []CallSpec{
				{Kind: "execute", InstanceID: strPtr("pair"), Sender: strPtr("alice"), Msg: map[string]any{"send_to": "bob"}, Funds: []CoinSpec{{Denom: "stake", Amount: "5"}}},
				{Kind: "bank_send", From: strPtr("alice"), To: strPtr("bob"), Coins: []CoinSpec{{Denom: "stake", Amount: "2"}}},
			},
		}},
	}}
	if err := bench.prepareWorkloadCalls(blocks); err != nil {
		t.Fatal(err)
	}
	pc, err := bench.preparedCall(blocks[0], blocks[0].Transactions[0], 0)
	if err != nil {
		t.Fatal(err)
	}
	want := []byte(`{"send_to":"` + to.String() + `"}`)
	if !bytes.Equal(pc.msg, want) {
		t.Fatalf("prepared msg=%s want=%s", pc.msg, want)
	}
	if len(pc.funds) != 1 || pc.funds[0].Denom != "stake" || pc.funds[0].Amount.String() != "5" {
		t.Fatalf("prepared funds=%v", pc.funds)
	}
	bank, err := bench.preparedCall(blocks[0], blocks[0].Transactions[0], 1)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(bank.from, sender) || !bytes.Equal(bank.to, to) {
		t.Fatalf("prepared bank endpoints from=%s to=%s", bank.from, bank.to)
	}
	if len(bank.coins) != 1 || bank.coins[0].Amount.String() != "2" {
		t.Fatalf("prepared bank coins=%v", bank.coins)
	}
}
