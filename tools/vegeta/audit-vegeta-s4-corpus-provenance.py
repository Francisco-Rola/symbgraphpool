#!/usr/bin/env python3
"""Audit frozen Vegeta S4 corpus identity and explain transaction-count provenance.

This is deliberately independent of the native translation.  It validates internal corpus
identity (range, block uniqueness, transaction hashes/indices) and reports the published Vegeta
S4 transaction count as an external comparison only.  A mismatch with the paper is recorded, not
silently normalized and not treated as an internal-integrity failure.
"""
from __future__ import annotations

import argparse
import json
import math
from collections import Counter
from pathlib import Path
from statistics import median
from typing import Any

from native_s3_planner_compat import normalize_address, runtime_code_family
from vegeta_corpus import iter_blocks


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def percentile(values: list[int], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = q * (len(ordered) - 1)
    lo = math.floor(rank)
    hi = math.ceil(rank)
    if lo == hi:
        return float(ordered[lo])
    return ordered[lo] * (hi - rank) + ordered[hi] * (rank - lo)


def code_present(code_cache: dict[str, Any], address: str) -> bool | None:
    addr = normalize_address(address)
    if not addr:
        return None
    row = code_cache.get(addr)
    if row is None:
        return None
    code = str((row or {}).get("code") or "0x")
    return runtime_code_family(code) is not None


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--expected-blocks", type=int, default=5000)
    ap.add_argument("--expected-start", type=int, default=18_581_726)
    ap.add_argument("--expected-end", type=int, default=18_586_725)
    ap.add_argument("--published-transactions", type=int, default=747_651)
    ns = ap.parse_args()

    code_cache = read_json(ns.code_cache)
    seen_blocks: set[int] = set()
    seen_tx_hashes: Counter[str] = Counter()
    seen_block_hashes: Counter[tuple[int, str]] = Counter()
    duplicate_positions: list[dict[str, Any]] = []
    block_counts: list[int] = []
    failed = successful = 0
    kinds: Counter[str] = Counter()
    total_gas = 0
    first = None
    last = None

    for block in iter_blocks(ns.corpus):
        bn = int(block["block_number"])
        if first is None:
            first = bn
        last = bn
        if bn in seen_blocks:
            duplicate_positions.append({"kind": "duplicate-block-number", "block_number": bn})
        seen_blocks.add(bn)
        txs = block.get("transactions") or []
        block_counts.append(len(txs))
        seen_indices: set[int] = set()
        for pos, tx in enumerate(txs):
            idx = int(tx.get("tx_index", -1))
            if idx in seen_indices or idx != pos:
                duplicate_positions.append({"kind": "bad-tx-index", "block_number": bn, "position": pos, "tx_index": idx})
            seen_indices.add(idx)
            tx_hash = str(tx.get("tx_hash") or "").lower()
            if not tx_hash:
                duplicate_positions.append({"kind": "missing-tx-hash", "block_number": bn, "position": pos})
            else:
                seen_tx_hashes[tx_hash] += 1
                seen_block_hashes[(bn, tx_hash)] += 1
            total_gas += int(tx.get("gas_used", 0) or 0)
            if bool(tx.get("failed")):
                failed += 1
            else:
                successful += 1

            to = str(tx.get("to") or "<create>").lower()
            selector = str(tx.get("selector") or "0x").lower()
            if to == "<create>":
                kinds["contract_create"] += 1
            elif selector != "0x":
                kinds["calldata_contract_call"] += 1
            else:
                present = code_present(code_cache, to)
                if present is True:
                    kinds["zero_calldata_contract_call"] += 1
                elif present is False:
                    kinds["plain_value_or_eoa_transfer"] += 1
                else:
                    kinds["zero_calldata_unknown_code"] += 1

    blocks = len(block_counts)
    transactions = sum(block_counts)
    duplicate_hashes = sorted((h, c) for h, c in seen_tx_hashes.items() if c > 1)
    duplicate_block_hashes = sorted((bn, h, c) for (bn, h), c in seen_block_hashes.items() if c > 1)
    expected_numbers = set(range(ns.expected_start, ns.expected_end + 1))
    missing_blocks = sorted(expected_numbers - seen_blocks)
    unexpected_blocks = sorted(seen_blocks - expected_numbers)
    internal_errors = []
    if blocks != ns.expected_blocks:
        internal_errors.append(f"block count {blocks} != expected {ns.expected_blocks}")
    if first != ns.expected_start or last != ns.expected_end:
        internal_errors.append(f"range {first}..{last} != expected {ns.expected_start}..{ns.expected_end}")
    if missing_blocks:
        internal_errors.append(f"missing expected blocks: {len(missing_blocks)}")
    if unexpected_blocks:
        internal_errors.append(f"unexpected blocks: {len(unexpected_blocks)}")
    if duplicate_hashes:
        internal_errors.append(f"duplicate tx hashes: {len(duplicate_hashes)}")
    if duplicate_block_hashes:
        internal_errors.append(f"duplicate (block,tx_hash) tuples: {len(duplicate_block_hashes)}")
    if duplicate_positions:
        internal_errors.append(f"block/tx position integrity errors: {len(duplicate_positions)}")

    published_delta = transactions - ns.published_transactions
    published_delta_pct = published_delta / ns.published_transactions if ns.published_transactions else 0.0
    report = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "internal_integrity": {
            "pass": not internal_errors,
            "errors": internal_errors,
            "expected_blocks": ns.expected_blocks,
            "expected_start_block": ns.expected_start,
            "expected_end_block": ns.expected_end,
            "observed_blocks": blocks,
            "observed_start_block": first,
            "observed_end_block": last,
            "missing_blocks": missing_blocks[:100],
            "unexpected_blocks": unexpected_blocks[:100],
            "unique_transaction_hashes": len(seen_tx_hashes),
            "duplicate_transaction_hashes": [{"tx_hash": h, "occurrences": c} for h, c in duplicate_hashes[:100]],
            "duplicate_block_transaction_hashes": [{"block_number": bn, "tx_hash": h, "occurrences": c} for bn, h, c in duplicate_block_hashes[:100]],
            "position_errors": duplicate_positions[:100],
        },
        "transactions": transactions,
        "successful_transactions": successful,
        "failed_transactions": failed,
        "total_gas_used": total_gas,
        "per_block_transactions": {
            "min": min(block_counts) if block_counts else 0,
            "median": median(block_counts) if block_counts else 0,
            "p95": percentile(block_counts, 0.95),
            "max": max(block_counts) if block_counts else 0,
        },
        "transaction_classes": dict(kinds),
        "published_comparison": {
            "reference": "Vegeta NSDI'25 Table 2 S4 transaction count",
            "published_transactions": ns.published_transactions,
            "observed_transactions": transactions,
            "delta_transactions": published_delta,
            "delta_fraction_of_published": published_delta_pct,
            "matches": transactions == ns.published_transactions,
            "policy": "external provenance diagnostic only; never mutate/drop frozen transactions to force a match",
        },
    }
    atomic_json(ns.output, report)
    p = report["per_block_transactions"]
    lines = [
        "Vegeta S4 frozen corpus provenance audit",
        "",
        f"internal integrity: {'PASS' if not internal_errors else 'FAIL'}",
        f"blocks: {blocks} ({first}..{last})",
        f"transactions: {transactions} unique_hashes={len(seen_tx_hashes)}",
        f"success/fail: {successful}/{failed}",
        f"tx/block: min={p['min']} median={p['median']:.1f} p95={p['p95']:.1f} max={p['max']}",
        f"total gas used: {total_gas}",
        "transaction classes: " + ", ".join(f"{k}={v}" for k, v in sorted(kinds.items())),
        "",
        f"published Vegeta S4 tx count: {ns.published_transactions}",
        f"frozen corpus tx count: {transactions}",
        f"difference: {published_delta:+d} ({100*published_delta_pct:+.2f}% of published)",
        "policy: record this difference as provenance; do not rewrite the frozen corpus to match the paper.",
    ]
    if internal_errors:
        lines += ["", "Internal integrity errors:"] + [f"  - {x}" for x in internal_errors]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    if internal_errors:
        raise SystemExit("S4 frozen corpus failed internal provenance/integrity checks")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
