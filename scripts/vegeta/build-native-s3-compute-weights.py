#!/usr/bin/env python3
"""Build source-cost weights for deterministic native S3 compute calibration.

This is evaluation metadata only. It joins the already-frozen native execution plan to the frozen
EVM trace files by block/index/hash and emits opcode-step/gas weights. It never imports EVM storage
keys into the native scheduler or contract translation.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def load_plan(path: Path):
    rows = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        block = json.loads(line)
        bn = int(block["block_number"])
        for tx in block["transactions"]:
            key = (bn, int(tx["tx_index"]))
            rows[key] = str(tx.get("tx_hash", "")).lower()
    return rows


def load_traces(root: Path):
    rows = {}
    for block_dir in sorted(root.iterdir()):
        if not block_dir.is_dir() or not block_dir.name.isdigit():
            continue
        bn = int(block_dir.name)
        for path in block_dir.glob("*.json"):
            try:
                idx = int(path.name.split("-", 1)[0])
            except ValueError:
                continue
            obj = json.loads(path.read_text(encoding="utf-8"))
            result = obj.get("result", {})
            rows[(bn, idx)] = {
                "tx_hash": str(obj.get("tx_hash", "")).lower(),
                "source_opcode_steps": int(result.get("steps") or 0),
                "source_gas_used": int(result.get("gasUsed") or 0),
            }
    return rows


def main():
    ap = argparse.ArgumentParser(description="Build Vegeta S3 source compute calibration weights.")
    ap.add_argument("--execution-plan", required=True)
    ap.add_argument("--source-traces-dir", required=True)
    ap.add_argument("--output", required=True)
    ap.add_argument("--summary", required=True)
    ap.add_argument("--max-missing-source", type=int, default=0)
    args = ap.parse_args()

    plan = load_plan(Path(args.execution_plan))
    traces = load_traces(Path(args.source_traces_dir))
    missing = sorted(set(plan) - set(traces))
    if len(missing) > args.max_missing_source:
        raise SystemExit(
            f"missing source transactions: {len(missing)} exceeds allowance {args.max_missing_source}"
        )
    extra = sorted(set(traces) - set(plan))
    hash_mismatches = []
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    total_steps = 0
    total_gas = 0
    weighted = 0
    with out.open("w", encoding="utf-8") as f:
        for key in sorted(plan):
            bn, idx = key
            tx_hash = plan[key]
            trace = traces.get(key)
            present = trace is not None
            steps = trace["source_opcode_steps"] if trace else None
            gas = trace["source_gas_used"] if trace else None
            source_hash = trace["tx_hash"] if trace else ""
            if present and tx_hash and source_hash and tx_hash != source_hash:
                hash_mismatches.append({
                    "block_number": bn,
                    "tx_index": idx,
                    "native_tx_hash": tx_hash,
                    "source_tx_hash": source_hash,
                })
            if present:
                weighted += 1
                total_steps += int(steps or 0)
                total_gas += int(gas or 0)
            f.write(json.dumps({
                "block_number": bn,
                "tx_index": idx,
                "tx_hash": tx_hash,
                "source_trace_present": present,
                "source_opcode_steps": steps,
                "source_gas_used": gas,
            }, sort_keys=True) + "\n")
    if hash_mismatches:
        raise SystemExit(f"source/native tx hash mismatches: {len(hash_mismatches)}")
    summary = {
        "schema_version": 1,
        "transactions": len(plan),
        "weighted_transactions": weighted,
        "missing_source_transactions": len(missing),
        "missing_source_details": [
            {"block_number": bn, "tx_index": idx, "tx_hash": plan[(bn, idx)]}
            for bn, idx in missing
        ],
        "extra_source_transactions": len(extra),
        "hash_mismatches": 0,
        "source_opcode_steps_total": total_steps,
        "source_gas_used_total": total_gas,
    }
    summary_path = Path(args.summary)
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    summary_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(
        f"compute weights: tx={len(plan)} matched={weighted} missing={len(missing)} "
        f"steps={total_steps} gas={total_gas}"
    )


if __name__ == "__main__":
    main()
