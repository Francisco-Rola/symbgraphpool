#!/usr/bin/env python3
"""Phase 5 summary using the consensus-pipelined system model as the primary view."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter, defaultdict
from pathlib import Path

from consensus_pipeline_metrics import consensus_pipeline_metrics
from typing import Any, Iterable


def load(path: Path) -> list[dict[str, Any]]:
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            out.append(json.loads(line))
    return out


def p(record: dict[str, Any], key: str) -> str:
    return record["metadata"]["parameters"].get(key, "-")


def med(values: Iterable[float]) -> float:
    values = list(values)
    return statistics.median(values) if values else float("nan")


def total_work_speed(record: dict[str, Any]) -> float:
    timing = record["pipeline_timing"]
    return timing["serial_reference_execution_nanos"] / timing["total_adaptive_block_nanos"]


def consensus_metrics(record: dict[str, Any], preconsensus_window_ms: float | None = None) -> dict[str, float]:
    metrics = consensus_pipeline_metrics(record, preconsensus_window_ms)
    return {
        "eligible_pre_nanos": float(metrics["preconsensus_eligible_nanos"]),
        "pre_nanos": float(metrics["preconsensus_completed_nanos"]),
        "spill_nanos": float(metrics["preconsensus_spill_nanos"]),
        "intrinsic_post_nanos": float(metrics["intrinsic_postconsensus_nanos"]),
        "post_nanos": float(metrics["postconsensus_validation_nanos"]),
        "bottleneck_nanos": float(metrics["pipeline_bottleneck_nanos"]),
        "preexecution_complete": bool(metrics["preexecution_complete_before_consensus"]),
        "validation_speedup": float(metrics["validation_latency_speedup"]),
        "throughput_speedup": float(metrics["throughput_speedup"]),
        "serial_validation_nanos": float(metrics["serial_validation_latency_nanos"]),
        "serial_tps": float(metrics["serial_throughput_tps"]),
        "acg_tps": float(metrics["acg_throughput_tps"]),
    }

def summarize(records: list[dict[str, Any]], preconsensus_window_ms: float | None = None) -> str:
    lines: list[str] = []
    cm = lambda record: consensus_metrics(record, preconsensus_window_ms)
    experiments = Counter(r["metadata"]["experiment_id"] for r in records)
    lines.append(
        f"records={len(records)} schema_versions={sorted({r['schema_version'] for r in records})} "
        f"serial_equivalent={sum(r['correctness']['serial_equivalent'] is True for r in records)}/{len(records)} "
        f"bypassed={sum(bool(r['planning']['serial_bypassed']) for r in records)} "
        f"candidate_misses={sum(r['feedback']['candidate_misses'] for r in records)}"
    )
    lines.append("experiments=" + ", ".join(f"{name}:{count}" for name, count in sorted(experiments.items())))
    lines.append("")
    lines.append("=== Primary consensus-window-aware metrics ===")
    if preconsensus_window_ms is None:
        lines.append("pre-consensus window=unbounded: all eligible pre-execution is assumed complete before consensus")
    else:
        lines.append(f"pre-consensus window={preconsensus_window_ms:g}ms; unfinished eligible work spills after consensus")
    lines.append(
        "validation latency: serial=full canonical execution after consensus; "
        "ACG=unfinished pre-execution spill + synchronous reconciliation/validation/replay/feedback"
    )
    lines.append(
        "throughput: serial service time=serial validation latency; "
        "ACG service time=max(pre-consensus stage, post-consensus stage)"
    )
    lines.append("total-work speedup is retained only as a secondary non-overlapped diagnostic")
    speculative = [r for r in records if not r["planning"]["serial_bypassed"]]
    bypassed = [r for r in records if r["planning"]["serial_bypassed"]]
    for label, rows in (("speculative", speculative), ("serial-fallback", bypassed), ("all", records)):
        if not rows:
            continue
        lines.append(
            f"{label:15s} n={len(rows):3d} "
            f"validation(acg/serial)={med(cm(r)['post_nanos'] for r in rows)/1e6:.3f}/"
            f"{med(cm(r)['serial_validation_nanos'] for r in rows)/1e6:.3f}ms "
            f"speedup={med(cm(r)['validation_speedup'] for r in rows):.2f}x "
            f"pre/post={med(cm(r)['pre_nanos'] for r in rows)/1e6:.3f}/"
            f"{med(cm(r)['post_nanos'] for r in rows)/1e6:.3f}ms "
            f"bottleneck={med(cm(r)['bottleneck_nanos'] for r in rows)/1e6:.3f}ms "
            f"throughput-speedup={med(cm(r)['throughput_speedup'] for r in rows):.2f}x "
            f"total-work={med(total_work_speed(r) for r in rows):.2f}x"
        )

    lines.append("")
    lines.append("=== Phase 5 control-plane medians ===")
    control = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase5-control-plane"]
    groups: dict[tuple[str, ...], list[dict[str, Any]]] = defaultdict(list)
    for r in control:
        key = (
            r["metadata"]["mode"],
            p(r, "prediction_quality"),
            p(r, "acg.serial_bypass_enabled"),
            p(r, "complexity"),
            p(r, "contention"),
        )
        groups[key].append(r)
    for key in sorted(groups):
        mode, prediction, bypass_cfg, complexity, contention = key
        rows = groups[key]
        nonzero = [r for r in rows if r["scheduling"]["candidate_edges"] > 0]
        compression = med(
            r["scheduling"]["candidate_edges"] / max(1, r["scheduling"]["materialized_candidate_edges"])
            for r in nonzero
        )
        lines.append(
            f"mode={mode:16s} pred={prediction:8s} bypass_cfg={bypass_cfg:5s} cx={complexity:6s} cont={contention:5s} "
            f"bypass={sum(r['planning']['serial_bypassed'] for r in rows)}/{len(rows)} "
            f"plan={med(r['pipeline_timing']['planning_nanos'] for r in rows)/1e6:.2f}ms "
            f"feedback={med(r['feedback_timing']['total_nanos'] for r in rows)/1e6:.2f}ms matC={compression:.1f}x "
            f"validation={med(cm(r)['validation_speedup'] for r in rows):.2f}x "
            f"throughput={med(cm(r)['throughput_speedup'] for r in rows):.2f}x "
            f"total-work={med(total_work_speed(r) for r in rows):.2f}x"
        )

    lines.append("")
    lines.append("=== B512 bucketed adaptive/no-bypass stress ===")
    stress = [
        r for r in control
        if p(r, "sim.block_size") == "512"
        and p(r, "prediction_quality") == "bucketed"
        and p(r, "acg.serial_bypass_enabled") == "false"
        and r["metadata"]["mode"] in {"probability-only", "cost-aware"}
    ]
    stress_groups: dict[tuple[str, str, str], list[dict[str, Any]]] = defaultdict(list)
    for r in stress:
        stress_groups[(r["metadata"]["mode"], p(r, "complexity"), p(r, "contention"))].append(r)
    for key in sorted(stress_groups):
        rows = stress_groups[key]
        logical = med(r["scheduling"]["candidate_edges"] for r in rows)
        materialized = med(r["scheduling"]["materialized_candidate_edges"] for r in rows)
        lines.append(
            f"mode={key[0]:16s} cx={key[1]:6s} cont={key[2]:5s} "
            f"logical/materialized={logical:.0f}/{materialized:.0f} ({logical/max(1,materialized):.1f}x) "
            f"soft={med(r['scheduling']['soft_edges'] for r in rows):.0f} "
            f"pre/post={med(cm(r)['pre_nanos'] for r in rows)/1e6:.2f}/"
            f"{med(cm(r)['post_nanos'] for r in rows)/1e6:.2f}ms "
            f"validation={med(cm(r)['validation_speedup'] for r in rows):.2f}x "
            f"throughput={med(cm(r)['throughput_speedup'] for r in rows):.2f}x "
            f"replay={med(r['execution']['replayed_transactions'] for r in rows):.1f} "
            f"total-work={med(total_work_speed(r) for r in rows):.2f}x"
        )

    lines.append("")
    lines.append("=== Mixed-complexity admission ===")
    mixed = [r for r in records if r["metadata"]["experiment_id"] == "conflictlab-phase5-mixed-admission"]
    mix_groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for r in mixed:
        mix_groups[(p(r, "complexity_mix"), p(r, "contention"))].append(r)
    for key in sorted(mix_groups):
        rows = mix_groups[key]
        lines.append(
            f"mix={key[0]:8s} cont={key[1]:5s} "
            f"bypass={sum(r['planning']['serial_bypassed'] for r in rows)}/{len(rows)} "
            f"validation={med(cm(r)['validation_speedup'] for r in rows):.2f}x "
            f"throughput={med(cm(r)['throughput_speedup'] for r in rows):.2f}x "
            f"total-work={med(total_work_speed(r) for r in rows):.2f}x"
        )

    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--preconsensus-window-ms",
        type=float,
        help="pre-consensus work budget; omitted assumes all eligible pre-execution completes before consensus",
    )
    args = parser.parse_args()
    text = summarize(load(args.records), args.preconsensus_window_ms)
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
