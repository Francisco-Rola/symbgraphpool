#!/usr/bin/env python3
"""Build deterministic source-cost weights for native Vegeta execution.

When exact execution traces are available, opcode steps remain the preferred metric.  Large
workloads such as S1 can instead use the canonical transaction ``gas_used`` proxy preserved in the
native execution plan.  The selected metric is written into every weight row so result records do
not mislabel gas-based calibration as opcode-step calibration.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def load_plan(path: Path, max_blocks: int = 0):
    rows = {}
    with path.open(encoding="utf-8") as f:
        seen_blocks = 0
        for line in f:
            if not line.strip(): continue
            if max_blocks and seen_blocks >= max_blocks: break
            block = json.loads(line); bn = int(block["block_number"]); seen_blocks += 1
            for tx in block["transactions"]:
                rows[(bn, int(tx["tx_index"]))] = {
                    "tx_hash": str(tx.get("tx_hash", "")).lower(),
                    "gas_proxy": int(tx.get("source_compute_proxy") or 0),
                }
    return rows


def load_traces(root: Path):
    rows = {}
    for block_dir in sorted(root.iterdir()):
        if not block_dir.is_dir() or not block_dir.name.isdigit(): continue
        bn = int(block_dir.name)
        for path in block_dir.glob("*.json"):
            try: idx = int(path.name.split("-", 1)[0])
            except ValueError: continue
            obj = json.loads(path.read_text(encoding="utf-8")); result = obj.get("result", {})
            rows[(bn, idx)] = {
                "tx_hash": str(obj.get("tx_hash", "")).lower(),
                "units": int(result.get("steps") or 0),
                "gas": int(result.get("gasUsed") or 0),
            }
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--execution-plan", required=True)
    ap.add_argument("--source-traces-dir")
    ap.add_argument("--fallback-plan-gas", action="store_true")
    ap.add_argument("--output", required=True)
    ap.add_argument("--summary", required=True)
    ap.add_argument("--max-missing-source", type=int, default=0)
    ap.add_argument("--max-blocks", type=int, default=0, help="process only the first N execution-plan blocks (0 = all)")
    args = ap.parse_args()
    if args.max_blocks < 0:
        raise SystemExit("--max-blocks must be non-negative")
    if not args.source_traces_dir and not args.fallback_plan_gas:
        raise SystemExit("provide --source-traces-dir or --fallback-plan-gas")

    if args.fallback_plan_gas and not args.source_traces_dir:
        out = Path(args.output); out.parent.mkdir(parents=True, exist_ok=True)
        transactions = weighted = total_units = 0
        with Path(args.execution_plan).open(encoding="utf-8") as source, out.open("w", encoding="utf-8") as target:
            seen_blocks = 0
            for line in source:
                if not line.strip(): continue
                if args.max_blocks and seen_blocks >= args.max_blocks: break
                block = json.loads(line); bn = int(block["block_number"]); seen_blocks += 1
                for tx in block.get("transactions") or []:
                    transactions += 1; weighted += 1
                    units = int(tx.get("source_compute_proxy") or 0); total_units += units
                    target.write(json.dumps({
                        "block_number": bn, "tx_index": int(tx["tx_index"]),
                        "tx_hash": str(tx.get("tx_hash", "")).lower(),
                        "source_trace_present": False, "source_compute_present": True,
                        "source_compute_metric": "gas_used", "source_compute_units": units,
                        "source_opcode_steps": None, "source_gas_used": units,
                    }, sort_keys=True) + "\n")
        summary = {
            "schema_version": 2, "transactions": transactions, "weighted_transactions": weighted,
            "compute_metric": "gas_used", "missing_source_transactions": 0,
            "extra_source_transactions": 0, "hash_mismatches": 0,
            "source_compute_units_total": total_units, "source_opcode_steps_total": 0, "source_gas_used_total": total_units,
            "fallback_plan_gas": True, "memory_mode": "streaming",
        }
        Path(args.summary).write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        print(f"compute weights: tx={transactions} weighted={weighted} metric=gas_used units={total_units} missing=0")
        return

    plan = load_plan(Path(args.execution_plan), args.max_blocks)
    traces = load_traces(Path(args.source_traces_dir)) if args.source_traces_dir else {}
    metric = "opcode_steps" if args.source_traces_dir else "gas_used"
    missing = sorted(set(plan) - set(traces)) if args.source_traces_dir else []
    if args.source_traces_dir and len(missing) > args.max_missing_source:
        raise SystemExit(f"missing source transactions: {len(missing)} exceeds allowance {args.max_missing_source}")
    extra = sorted(set(traces) - set(plan)) if args.source_traces_dir else []
    hash_mismatches = []
    out = Path(args.output); out.parent.mkdir(parents=True, exist_ok=True)
    total_units = total_gas = weighted = 0
    with out.open("w", encoding="utf-8") as f:
        for key in sorted(plan):
            bn, idx = key; meta = plan[key]; trace = traces.get(key)
            if trace is not None:
                units = int(trace["units"]); gas = int(trace["gas"]); source_hash = trace["tx_hash"]
                if meta["tx_hash"] and source_hash and meta["tx_hash"] != source_hash:
                    hash_mismatches.append(key)
            elif args.fallback_plan_gas:
                units = int(meta["gas_proxy"]); gas = units; source_hash = meta["tx_hash"]
            else:
                units = 0; gas = 0; source_hash = ""
            present = trace is not None or args.fallback_plan_gas
            if present:
                weighted += 1; total_units += units; total_gas += gas
            f.write(json.dumps({
                "block_number": bn, "tx_index": idx, "tx_hash": meta["tx_hash"],
                "source_trace_present": trace is not None,
                "source_compute_present": present,
                "source_compute_metric": metric,
                "source_compute_units": units if present else None,
                "source_opcode_steps": units if metric == "opcode_steps" and present else None,
                "source_gas_used": gas if present else None,
            }, sort_keys=True) + "\n")
    if hash_mismatches: raise SystemExit(f"source/native tx hash mismatches: {len(hash_mismatches)}")
    summary = {
        "schema_version": 2, "transactions": len(plan), "weighted_transactions": weighted,
        "compute_metric": metric, "missing_source_transactions": len(missing),
        "extra_source_transactions": len(extra), "hash_mismatches": 0,
        "source_compute_units_total": total_units, "source_opcode_steps_total": total_units if metric == "opcode_steps" else 0,
        "source_gas_used_total": total_gas,
        "fallback_plan_gas": bool(args.fallback_plan_gas and not args.source_traces_dir),
    }
    Path(args.summary).write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(f"compute weights: tx={len(plan)} weighted={weighted} metric={metric} units={total_units} missing={len(missing)}")


if __name__ == "__main__": main()
