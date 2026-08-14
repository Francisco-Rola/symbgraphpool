#!/usr/bin/env python3
"""Human-readable summary for production block-size and coarse-policy evaluation."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path


def load(path: Path):
    records = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            records.append(json.loads(line))
    return records


def get(record, path, default=0):
    value = record
    for part in path.split("."):
        if not isinstance(value, dict) or part not in value:
            return default
        value = value[part]
    return default if value is None else value


def median(records, path):
    values = [float(get(record, path)) for record in records]
    return statistics.median(values) if values else 0.0


def ratio(num, den):
    return num / den if den else 0.0


def fmt_ms(nanos):
    return f"{nanos / 1e6:.3f} ms"


def block_size(record):
    params = record.get("metadata", {}).get("parameters", {})
    return int(params.get("sim.block_size", params.get("transactions", "0")))


def pipeline_speedup(records):
    milli = median(records, "pipeline_timing.end_to_end_speedup_milli")
    return milli / 1000.0 if milli else 0.0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    records = load(args.records)
    lines = [
        f"records={len(records)} schema_versions={sorted({r.get('schema_version') for r in records})}",
        "block_sizes=" + ",".join(map(str, sorted({block_size(r) for r in records}))),
    ]

    control = [
        r
        for r in records
        if r.get("metadata", {}).get("experiment_id")
        == "conflictlab-control-plane-regression"
    ]
    if control:
        lines += ["", "=== exact prediction / production block-size scaling ==="]
        groups = defaultdict(list)
        for record in control:
            p = record["metadata"]["parameters"]
            groups[(block_size(record), record["metadata"]["mode"], p.get("contention", "?"))].append(record)
        for key in sorted(groups):
            size, mode, contention = key
            samples = groups[key]
            candidate = median(samples, "scheduling.candidate_edges")
            pre = median(samples, "scheduling.pre_reduction_dependencies")
            scheduled = median(samples, "scheduling.scheduled_dependencies")
            raw_feedback = median(samples, "feedback.positive_observations") + median(
                samples, "feedback.negative_observations"
            )
            feedback_batches = median(samples, "feedback.observation_batches_applied")
            feedback_ns = median(samples, "feedback_timing.total_nanos")
            wall = median(samples, "parallelism.actual_execution_wall_nanos")
            serial = median(samples, "parallelism.serial_equivalent_work_nanos")
            pipeline_ns = median(samples, "pipeline_timing.total_adaptive_block_nanos")
            corrected = median(samples, "parallelism.scheduler_realization_corrected_milli") / 1000.0
            lines.append(
                f"B{size:<3d} {mode:16s} {contention:4s} candidate={candidate:.0f} "
                f"pre={pre:.0f} scheduled={scheduled:.0f} compression={ratio(pre, scheduled):.1f}x "
                f"feedback={fmt_ms(feedback_ns)} feedback/obs={ratio(feedback_ns, raw_feedback) / 1e3:.3f}us "
                f"raw/batch={ratio(raw_feedback, feedback_batches):.1f}x "
                f"executor={ratio(serial, wall):.2f}x scheduler={corrected:.3f}x "
                f"pipeline={pipeline_speedup(samples):.2f}x total={fmt_ms(pipeline_ns)}"
            )

    coarse = [
        r
        for r in records
        if r.get("metadata", {}).get("experiment_id")
        in {"conflictlab-coarse-policy-risk-sweep", "conflictlab-forced-speculation"}
    ]
    if coarse:
        lines += ["", "=== coarse prediction / risk-budget policy sweep ==="]
        groups = defaultdict(list)
        for record in coarse:
            p = record["metadata"]["parameters"]
            groups[
                (
                    block_size(record),
                    p.get("contention", p.get("hot_account_probability_bps", "?")),
                    p.get("warmup_blocks", "?"),
                    p.get("acg.risk_budget", "?"),
                    record["metadata"]["mode"],
                )
            ].append(record)
        for key in sorted(groups, key=lambda key: (key[0], key[1], int(key[2]), float(key[3]), key[4])):
            size, contention, warmup, budget, mode = key
            samples = groups[key]
            low = median(samples, "scheduling.low_edges")
            soft = median(samples, "scheduling.soft_edges")
            hard = median(samples, "scheduling.hard_edges")
            waves = median(samples, "scheduling.wave_count")
            deps = median(samples, "scheduling.scheduled_dependencies")
            replayed = median(samples, "execution.replayed_transactions")
            misses = median(samples, "feedback.candidate_misses")
            feedback_ns = median(samples, "feedback_timing.total_nanos")
            raw_feedback = median(samples, "feedback.positive_observations") + median(
                samples, "feedback.negative_observations"
            )
            wall = median(samples, "parallelism.actual_execution_wall_nanos")
            serial = median(samples, "parallelism.serial_equivalent_work_nanos")
            pipeline_ns = median(samples, "pipeline_timing.total_adaptive_block_nanos")
            lines.append(
                f"B{size:<3d} {contention:5s} warmup={warmup:>1s} risk={float(budget):.2f} "
                f"{mode:16s} low/soft/hard={low:.0f}/{soft:.0f}/{hard:.0f} "
                f"waves={waves:.0f} deps={deps:.0f} replayed={replayed:.0f} misses={misses:.0f} "
                f"feedback={fmt_ms(feedback_ns)} feedback/obs={ratio(feedback_ns, raw_feedback) / 1e3:.3f}us "
                f"executor={ratio(serial, wall):.2f}x pipeline={pipeline_speedup(samples):.2f}x "
                f"total={fmt_ms(pipeline_ns)}"
            )

    lines += [
        "",
        "Upload this summary plus records.jsonl and aggregate/summary-wide.csv for analysis.",
    ]
    text = "\n".join(lines) + "\n"
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
