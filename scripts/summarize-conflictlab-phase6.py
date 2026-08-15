#!/usr/bin/env python3
"""Human-readable Phase 6 consensus-realism summary."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable


def load(path: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def p(r: dict[str, Any], key: str, default: str = "-") -> str:
    return r["metadata"]["parameters"].get(key, default)


def med(values: Iterable[float]) -> float:
    values = list(values)
    return statistics.median(values) if values else float("nan")


def ms(nanos: float) -> float:
    return nanos / 1e6


def sx(r: dict[str, Any], key: str) -> float:
    value = r["consensus"].get(key)
    return float(value) / 1000.0 if value is not None else float("nan")


def total_work(r: dict[str, Any]) -> float:
    t = r["pipeline_timing"]
    return t["serial_reference_execution_nanos"] / max(1, t["total_adaptive_block_nanos"])


def reuse_fraction(r: dict[str, Any]) -> float:
    return r["execution"].get("reused_results", 0) / max(1, r["consensus"]["decided_transactions"])


def cutoff_fraction(r: dict[str, Any]) -> float:
    return r["consensus"].get("receipts_ready_by_cutoff", 0) / max(1, r["consensus"]["candidate_transactions"])


def line_for(rows: list[dict[str, Any]]) -> str:
    return (
        f"n={len(rows)} pre/post={ms(med(r['consensus']['pre_consensus_nanos'] for r in rows)):.2f}/"
        f"{ms(med(r['consensus']['post_consensus_nanos'] for r in rows)):.2f}ms "
        f"bottleneck={ms(med(r['consensus']['bottleneck_nanos'] for r in rows)):.2f}ms "
        f"validation={med(sx(r, 'validation_latency_speedup_milli') for r in rows):.2f}x "
        f"throughput={med(sx(r, 'throughput_speedup_milli') for r in rows):.2f}x "
        f"ready={100*med(cutoff_fraction(r) for r in rows):.1f}% "
        f"reuse={100*med(reuse_fraction(r) for r in rows):.1f}% "
        f"replay={med(r['execution'].get('replayed_transactions', 0) for r in rows):.1f} "
        f"total-work={med(total_work(r) for r in rows):.2f}x"
    )


def summarize(records: list[dict[str, Any]]) -> str:
    lines: list[str] = []
    counts = Counter(r["metadata"]["experiment_id"] for r in records)
    lines.append(
        f"records={len(records)} serial_equivalent="
        f"{sum(r['correctness'].get('serial_equivalent') is True for r in records)}/{len(records)} "
        f"bypassed={sum(bool(r['planning'].get('serial_bypassed')) for r in records)} "
        f"candidate_misses={sum(r['feedback'].get('candidate_misses', 0) for r in records)}"
    )
    lines.append("experiments=" + ", ".join(f"{k}:{v}" for k, v in sorted(counts.items())))
    lines.append("")
    lines.append("PRIMARY: validation=serial full post-consensus execution / ACG measured post-consensus critical path")
    lines.append("PRIMARY: throughput=serial full execution / max(ACG measured pre-consensus phase, post-consensus phase)")
    lines.append("SECONDARY: total-work speedup keeps the non-overlapped full-ACG-wall comparison")

    lines.append("")
    lines.append("=== Full feature-state matrix (500ms, identical candidate/decision) ===")
    feature = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase6-feature-state"]
    groups: dict[tuple[str, ...], list[dict[str, Any]]] = defaultdict(list)
    for r in feature:
        groups[(r["metadata"]["mode"], p(r,"complexity"), p(r,"prediction_quality"), p(r,"contention"), p(r,"acg.serial_bypass_enabled"))].append(r)
    for key in sorted(groups):
        mode,cx,pred,cont,bypass=key
        rows=groups[key]
        logical=[r for r in rows if r["scheduling"].get("materialized_candidate_edges",0)>0]
        compression=med(r["scheduling"]["candidate_edges"]/r["scheduling"]["materialized_candidate_edges"] for r in logical)
        lines.append(
            f"mode={mode:16s} cx={cx:6s} pred={pred:8s} cont={cont:5s} bypass={bypass:5s} "
            f"actual-bypass={sum(r['planning']['serial_bypassed'] for r in rows)}/{len(rows)} "
            f"plan={ms(med(r['planning']['total_nanos'] for r in rows)):.2f}ms matC={compression:.1f}x {line_for(rows)}"
        )

    lines.append("")
    lines.append("=== Real cutoff sensitivity (B512 high/mixed, admission enabled) ===")
    cutoff = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase6-consensus-cutoff"]
    groups=defaultdict(list)
    for r in cutoff:
        groups[(p(r,"consensus_cutoff_ms"),r["metadata"]["mode"],p(r,"complexity"),p(r,"prediction_quality"),p(r,"contention"))].append(r)
    for key in sorted(groups, key=lambda k:(int(k[0]),k[1:])):
        rows=groups[key]
        lines.append(f"cutoff={key[0]:4s}ms mode={key[1]:16s} cx={key[2]:6s} pred={key[3]:8s} cont={key[4]:5s} " + line_for(rows))

    lines.append("")
    lines.append("=== Forced buffered serial pre-execution ===")
    serial = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase6-serial-preexecution-cutoff"]
    groups=defaultdict(list)
    for r in serial:
        groups[(p(r,"consensus_cutoff_ms"),p(r,"complexity"))].append(r)
    for key in sorted(groups, key=lambda k:(int(k[0]),k[1])):
        rows=groups[key]
        lines.append(
            f"cutoff={key[0]:4s}ms cx={key[1]:6s} cutoff-hit={sum(r['consensus']['cutoff_reached'] for r in rows)}/{len(rows)} "
            + line_for(rows)
        )

    lines.append("")
    lines.append("=== Candidate/decided block divergence ===")
    div = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase6-consensus-divergence"]
    groups=defaultdict(list)
    for r in div:
        groups[(p(r,"consensus_divergence"),r["metadata"]["mode"],p(r,"complexity"),p(r,"prediction_quality"),p(r,"contention"))].append(r)
    for key in sorted(groups):
        rows=groups[key]
        shared=med(r["consensus"]["shared_transactions"]/max(1,r["consensus"]["candidate_transactions"]) for r in rows)
        same=med(r["consensus"]["same_position_transactions"]/max(1,r["consensus"]["candidate_transactions"]) for r in rows)
        lines.append(
            f"div={key[0]:18s} mode={key[1]:16s} cx={key[2]:6s} pred={key[3]:8s} cont={key[4]:5s} "
            f"shared/same={100*shared:.1f}/{100*same:.1f}% " + line_for(rows)
        )

    lines.append("")
    lines.append("=== Policy sensitivity (B512 bucketed, admission disabled) ===")
    pol=[r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase6-policy-sensitivity"]
    groups=defaultdict(list)
    for r in pol:
        groups[(r["metadata"]["mode"],p(r,"acg.risk_budget"),p(r,"complexity"),p(r,"contention"))].append(r)
    for key in sorted(groups):
        rows=groups[key]
        lines.append(f"mode={key[0]:16s} risk={key[1]:4s} cx={key[2]:6s} cont={key[3]:5s} " + line_for(rows))

    lines.append("")
    lines.append("=== VM lifecycle sanity (B512 exact/static/25%) ===")
    lifecycle = [
        r for r in records
        if r["metadata"]["experiment_id"] == "conflictlab-phase6-vm-lifecycle-sanity"
    ]
    groups = defaultdict(list)
    for r in lifecycle:
        groups[(p(r, "complexity"), p(r, "vm_instance_lifecycle"))].append(r)
    for key in sorted(groups):
        rows = groups[key]
        acquires = sum(r["execution"]["contract"].get("wasm_instance_acquires", 0) for r in rows)
        vm_nanos = sum(r["execution"]["contract"].get("aggregate_wasm_instance_acquire_nanos", 0) for r in rows)
        vm_us_per_tx = vm_nanos / max(1, acquires) / 1e3
        lines.append(
            f"cx={key[0]:6s} lifecycle={key[1]:7s} vm={vm_us_per_tx:.1f}us/tx " + line_for(rows)
        )

    lines.append("")
    lines.append("=== Observed decided-block serial service time per transaction ===")
    for cx in ("low","medium","high","mixed"):
        rows=[r for r in feature if p(r,"complexity")==cx]
        if rows:
            us=med(r["consensus"]["serial_validation_latency_nanos"]/max(1,r["consensus"]["decided_transactions"])/1e3 for r in rows)
            lines.append(f"cx={cx:6s} median serial service={us:.1f}us/tx")
    return "\n".join(lines)+"\n"


def main() -> None:
    parser=argparse.ArgumentParser()
    parser.add_argument("records",type=Path)
    parser.add_argument("--output",type=Path)
    args=parser.parse_args()
    text=summarize(load(args.records))
    if args.output:
        args.output.write_text(text,encoding="utf-8")
    else:
        print(text,end="")


if __name__ == "__main__":
    main()
