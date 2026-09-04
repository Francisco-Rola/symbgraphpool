#!/usr/bin/env python3
"""Fail-closed Serial-vs-Vegeta comparison using Vegeta NSDI'25 replay throughput.

Primary metric follows the paper's single-node evaluation: throughput is total
transactions divided by the replay/post-consensus phase. Speculation/pre-work is
reported separately and is never silently folded into the primary throughput.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def read_rows(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{lineno}: invalid JSON: {exc}")
    if not rows:
        raise SystemExit(f"empty result file: {path}")
    return rows


def signature(row: dict[str, Any]) -> tuple[Any, ...]:
    return (
        row.get("dataset"), int(row.get("workers", -1)),
        float(row.get("compute_scale", -1)), float(row.get("go_iterations_per_nano", -1)),
        int(row.get("iavl_cache_size", -1)), bool(row.get("iavl_sync_pruning", False)),
        row.get("evaluator_sha256"),
    )


def domain(rows: list[dict[str, Any]]) -> list[tuple[int, int, int]]:
    return sorted((int(r.get("sample", -1)), int(r["block_number"]), int(r.get("transactions", -1))) for r in rows)


def nanos(rows: list[dict[str, Any]], key: str, fallback: str | None = None) -> int:
    total = 0
    for row in rows:
        value = int(row.get(key, 0) or 0)
        if value == 0 and fallback:
            value = int(row.get(fallback, 0) or 0)
        total += value
    return total


def sec(n: int) -> float:
    return n / 1e9


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--serial", required=True, type=Path)
    ap.add_argument("--vegeta", required=True, type=Path)
    args = ap.parse_args()

    serial = read_rows(args.serial)
    vegeta = read_rows(args.vegeta)
    if {r.get("strategy") for r in serial} != {"cosmos-wasmd-direct-serial"}:
        raise SystemExit("serial file contains non-Serial strategy rows")
    if {r.get("strategy") for r in vegeta} != {"cosmos-wasmd-vegeta"}:
        raise SystemExit("Vegeta file contains non-Vegeta strategy rows")
    if domain(serial) != domain(vegeta):
        raise SystemExit("Serial and Vegeta block/transaction domains differ")
    sigs = {signature(r) for r in serial + vegeta}
    if len(sigs) != 1:
        raise SystemExit(f"Serial/Vegeta configuration mismatch: {sorted(map(str, sigs))}")
    if not all(bool(r.get("serial_equivalent", False)) for r in vegeta):
        raise SystemExit("Vegeta contains a block that is not Serial-equivalent")

    txs = sum(int(r.get("transactions", 0)) for r in serial)
    blocks = len(serial)
    serial_ns = nanos(serial, "post_consensus_nanos", "strategy_total_nanos")
    vegeta_post_ns = nanos(vegeta, "post_consensus_nanos", "strategy_total_nanos")
    vegeta_pre_ns = nanos(vegeta, "pre_consensus_nanos")
    vegeta_work_ns = nanos(vegeta, "strategy_total_nanos")
    matched_ns = nanos(vegeta, "matched_serial_nanos")
    longest_sum = sum(int(r.get("vegeta_longest_chain", 0) or 0) for r in vegeta)
    replay = sum(int(r.get("reexecutions", 0) or 0) for r in vegeta)
    safety = sum(int(r.get("safety_replays", 0) or 0) for r in vegeta)
    fallback_blocks = sum(1 for r in vegeta if bool(r.get("vegeta_canonical_fallback", False)))
    alg3_ns = nanos(vegeta, "vegeta_alg3_validation_nanos")
    range_ns = nanos(vegeta, "vegeta_range_validation_nanos")
    intrinsic_ns = nanos(vegeta, "vegeta_intrinsic_reexecution_nanos")
    historical_fallback_ns = nanos(vegeta, "vegeta_historical_fallback_nanos")
    historical_fallback_txs = sum(int(r.get("vegeta_historical_fallback_transactions", 0) or 0) for r in vegeta)

    serial_tps = txs * 1e9 / serial_ns
    vegeta_replay_tps = txs * 1e9 / vegeta_post_ns
    vegeta_work_tps = txs * 1e9 / vegeta_work_ns
    replay_x = serial_ns / vegeta_post_ns
    work_x = serial_ns / vegeta_work_ns
    matched_x = matched_ns / vegeta_post_ns
    chain_ratio = txs / longest_sum if longest_sum else 0.0

    dataset, workers, scale, iter_ns, cache, sync, sha = next(iter(sigs))
    print("Vegeta paper-method Serial comparison")
    print(f"dataset              = {dataset}")
    print(f"blocks / tx          = {blocks} / {txs}")
    print(f"workers              = {workers}")
    print(f"compute              = gas_used x{scale:g}, iter/ns={iter_ns:.17g}")
    print(f"IAVL                 = cache={cache}, sync_pruning={str(sync).lower()}")
    print(f"evaluator_sha256     = {sha}")
    print()
    print("Primary single-node metric (Vegeta NSDI'25 Figure-10 convention):")
    print("  throughput = transactions / replay(post-consensus) time")
    print(f"serial execution     = {sec(serial_ns):.3f}s  {serial_tps:.1f} tx/s")
    print(f"vegeta replay        = {sec(vegeta_post_ns):.3f}s  {vegeta_replay_tps:.1f} tx/s")
    print(f"replay throughput-x  = {replay_x:.3f}x")
    print()
    print("Transparency / adaptation metrics:")
    print(f"vegeta speculation   = {sec(vegeta_pre_ns):.3f}s")
    print(f"vegeta active work   = {sec(vegeta_work_ns):.3f}s  {vegeta_work_tps:.1f} tx/s")
    print(f"active-work-x        = {work_x:.3f}x")
    print(f"matched replay-x     = {matched_x:.3f}x")
    print(f"reexecution          = {replay}/{txs} ({100.0*replay/txs:.2f}%)")
    print(f"safety replays       = {safety}")
    print(f"Algorithm-3 validation = {sec(alg3_ns):.3f}s")
    print(f"Wasmd range validation = {sec(range_ns):.3f}s")
    print(f"intrinsic reexecution  = {sec(intrinsic_ns):.3f}s")
    print(f"historical fallback    = {sec(historical_fallback_ns):.3f}s ({historical_fallback_txs} tx re-run outside paper replay)")
    print(f"canonical fallbacks  = {fallback_blocks}/{blocks} ({100.0*fallback_blocks/blocks:.2f}%)")
    print(f"dependency-chain ratio = {chain_ratio:.3f}x (tx / sum(block longest-chain lengths))")
    print()
    print("NOTE: replay throughput deliberately excludes pre-consensus speculation and our")
    print("historical-state fallback, matching Vegeta's single-node replay methodology.")
    print("active-work-x and historical fallback are reported separately so adaptation cost")
    print("is never hidden in our artifact.")


if __name__ == "__main__":
    main()
