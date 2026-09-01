#!/usr/bin/env python3
"""Prepare memory-bounded S1 metadata needed by the native Wasmd translation pipeline.

The full public-RPC corpus contains conservative storage access lists and is large enough that
loading all 5,000 blocks at once is undesirable.  This helper performs two streaming passes:

* writes ``thin-corpus.jsonl`` containing only transaction/block metadata needed by the call-plan
  translator (never concrete historical read/write keys), and
* records every direct destination / storage-owner address and its first-seen block so historical
  runtime bytecode can be fetched resumably without retaining the full corpus in memory.

The resulting thin corpus is safe to feed to the native planner because source accesses are kept in
``corpus.jsonl`` only for offline coverage accounting and never exposed to Rust-ACG prediction.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from vegeta_corpus import iter_blocks, storage_contract

TX_FIELDS = (
    "tx_index", "tx_hash", "from", "to", "selector", "input", "value", "gas_used",
    "opcode_steps", "failed",
)
BLOCK_FIELDS = ("schema_version", "block_number", "block_hash", "timestamp")


def compact_tx(tx: dict[str, Any]) -> dict[str, Any]:
    return {key: tx.get(key) for key in TX_FIELDS if key in tx}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--output-dir", type=Path, required=True)
    ns = ap.parse_args()

    ns.output_dir.mkdir(parents=True, exist_ok=True)
    thin_path = ns.output_dir / "thin-corpus.jsonl"
    address_path = ns.output_dir / "relevant-addresses.json"
    summary_path = ns.output_dir / "source-summary.json"

    first_seen: dict[str, int] = {}
    block_count = tx_count = read_records = write_records = 0
    first_block = last_block = None

    with thin_path.open("w", encoding="utf-8") as out:
        for block in iter_blocks(ns.corpus):
            bn = int(block["block_number"])
            block_count += 1
            first_block = bn if first_block is None else min(first_block, bn)
            last_block = bn if last_block is None else max(last_block, bn)
            thin = {key: block.get(key) for key in BLOCK_FIELDS if key in block}
            thin_txs = []
            for tx in block.get("transactions") or []:
                tx_count += 1
                to = str(tx.get("to") or "").lower()
                if to.startswith("0x") and len(to) == 42:
                    first_seen.setdefault(to, bn)
                for key in tx.get("reads") or []:
                    read_records += 1
                    owner = storage_contract(str(key))
                    if owner:
                        first_seen.setdefault("0x" + owner, bn)
                for key in tx.get("writes") or []:
                    write_records += 1
                    owner = storage_contract(str(key))
                    if owner:
                        first_seen.setdefault("0x" + owner, bn)
                thin_txs.append(compact_tx(tx))
            thin["transactions"] = thin_txs
            out.write(json.dumps(thin, sort_keys=True, separators=(",", ":")) + "\n")

    address_path.write_text(json.dumps({
        "schema_version": 1,
        "source_corpus": str(ns.corpus),
        "addresses": [
            {"address": address, "first_seen_block": first_seen[address]}
            for address in sorted(first_seen)
        ],
    }, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    summary = {
        "schema_version": 1,
        "source_corpus": str(ns.corpus),
        "blocks": block_count,
        "first_block": first_block,
        "last_block": last_block,
        "transactions": tx_count,
        "read_records": read_records,
        "write_records": write_records,
        "relevant_addresses": len(first_seen),
        "thin_corpus": str(thin_path),
        "relevant_address_file": str(address_path),
    }
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
