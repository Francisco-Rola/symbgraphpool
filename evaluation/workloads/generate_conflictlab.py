#!/usr/bin/env python3
"""Generate native ConflictLab Wasmd workloads used by publication experiments."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WASM = "benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm"
SYMBOLIC = ROOT / "benchmarks/symbolic/conflictlab.symbolic.json"


def tx_hash(tag: str, block: int, index: int) -> str:
    return "0x" + hashlib.sha256(f"{tag}:{block}:{index}".encode()).hexdigest()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--output-dir", type=Path, required=True)
    ap.add_argument("--blocks", type=int, default=10)
    ap.add_argument("--transactions", type=int, default=384)
    ap.add_argument("--lanes", type=int, default=384,
                    help="number of independent BALANCES keys per block; 1 is serial, tx count is conflict-free")
    ap.add_argument("--work-iterations", type=int, default=786_432)
    ap.add_argument("--storage-rounds", type=int, default=0)
    ap.add_argument("--payload-bytes", type=int, default=512)
    ap.add_argument("--amount", type=int, default=1)
    ap.add_argument("--tag", default="conflictlab-native")
    args = ap.parse_args()
    if args.blocks <= 0 or args.transactions <= 0 or args.work_iterations <= 0:
        raise SystemExit("blocks, transactions, and work-iterations must be positive")
    if not 1 <= args.lanes <= args.transactions:
        raise SystemExit("lanes must be in 1..transactions")
    if args.storage_rounds < 0 or args.payload_bytes < 0:
        raise SystemExit("storage-rounds/payload-bytes must be non-negative")
    if not SYMBOLIC.is_file():
        raise SystemExit(f"missing symbolic profile: {SYMBOLIC}")

    out = args.output_dir
    out.mkdir(parents=True, exist_ok=True)
    sym = out / "symbolic"
    sym.mkdir(exist_ok=True)
    shutil.copyfile(SYMBOLIC, sym / SYMBOLIC.name)

    payload_raw = bytes((i * 17 + 23) & 0xFF for i in range(args.payload_bytes))
    payload = base64.b64encode(payload_raw).decode("ascii")
    accounts = [f"native-s3-cl-account-{i:05d}" for i in range(args.lanes)]
    senders = [f"native-s3-cl-sender-{i:05d}" for i in range(args.transactions)]
    logical = ["native-s3-admin", *accounts, *senders]

    dataset = f"{args.tag}-lanes{args.lanes}-tx{args.transactions}"
    manifest = {
        "schema_version": 2,
        "dataset": dataset,
        "wasm_artifacts": {"conflictlab": WASM},
        "instances": [{
            "instance_id": "conflictlab",
            "family": "conflictlab",
            "instantiate_msg": {"admin": "native-s3-admin", "fee_bps": 0, "epoch": 0},
        }],
        "bank_seeds": [],
        "priming_calls": [],
        "logical_addresses": sorted(set(logical)),
        "blocks": args.blocks,
        "transactions": args.blocks * args.transactions,
        "first_timestamp": 1_800_000_000,
        "controlled_parallelism": {
            "lanes": args.lanes,
            "transactions_per_block": args.transactions,
            "ideal_transaction_parallelism": min(args.lanes, args.transactions),
            "work_iterations": args.work_iterations,
            "storage_rounds": args.storage_rounds,
            "payload_bytes": args.payload_bytes,
        },
    }
    (out / "execution-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")

    with (out / "execution-plan.jsonl").open("w") as f:
        for boff in range(args.blocks):
            block = boff + 1
            txs = []
            for i in range(args.transactions):
                lane = i % args.lanes
                txs.append({
                    "tx_index": i,
                    "tx_hash": tx_hash(dataset, block, i),
                    "source_failed": False,
                    "source_compute_proxy": 1,
                    "calls": [{
                        "kind": "execute",
                        "family": "conflictlab",
                        "instance_id": "conflictlab",
                        "sender": senders[i],
                        "msg": {"credit": {
                            "account": accounts[lane],
                            "amount": str(args.amount),
                            "work_iterations": args.work_iterations,
                            "storage_rounds": args.storage_rounds,
                            "payload": payload,
                        }},
                        "funds": [],
                    }],
                })
            f.write(json.dumps({
                "block_number": block,
                "timestamp": 1_800_000_000 + boff * 6,
                "transactions": txs,
            }, sort_keys=True) + "\n")

    meta = {
        "schema_version": 1,
        "dataset": dataset,
        "blocks": args.blocks,
        "transactions_per_block": args.transactions,
        "lanes": args.lanes,
        "ideal_transaction_parallelism": min(args.lanes, args.transactions),
        "conflict_free": args.lanes == args.transactions,
        "compute_source": "real ConflictLab deterministic_work in Wasm; evaluator synthetic compute must be zero",
    }
    (out / "workload.json").write_text(json.dumps(meta, indent=2, sort_keys=True) + "\n")
    print(f"generated {dataset}: blocks={args.blocks} tx/block={args.transactions} lanes={args.lanes}")


if __name__ == "__main__":
    main()
