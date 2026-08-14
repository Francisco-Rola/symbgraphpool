#!/usr/bin/env python3
"""Human-readable summary for the control-plane + forced-speculation rerun."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path


def load(path: Path):
    out = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            out.append(json.loads(line))
    return out


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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    records = load(args.records)
    lines = [f"records={len(records)} schema_versions={sorted({r.get('schema_version') for r in records})}"]

    control = [r for r in records if r.get("metadata", {}).get("experiment_id") == "conflictlab-control-plane-regression"]
    if control:
        lines += ["", "=== exact prediction / control-plane regression ==="]
        groups = defaultdict(list)
        for record in control:
            p = record["metadata"]["parameters"]
            groups[(record["metadata"]["mode"], p.get("contention", "?"))].append(record)
        for key in sorted(groups):
            mode, contention = key
            samples = groups[key]
            candidate = median(samples, "scheduling.candidate_edges")
            pre = median(samples, "scheduling.pre_reduction_dependencies")
            scheduled = median(samples, "scheduling.scheduled_dependencies")
            elided = median(samples, "scheduling.edges_elided_by_reduction")
            raw_feedback = median(samples, "feedback.positive_observations") + median(samples, "feedback.negative_observations")
            feedback_batches = median(samples, "feedback.observation_batches_applied")
            serialization_obs = median(samples, "feedback.serialization_cost_observations")
            serialization_batches = median(samples, "feedback.serialization_cost_batches_applied")
            feedback_ns = median(samples, "feedback_timing.total_nanos")
            wall = median(samples, "parallelism.actual_execution_wall_nanos")
            serial = median(samples, "parallelism.serial_equivalent_work_nanos")
            corrected = median(samples, "parallelism.scheduler_realization_corrected_milli") / 1000.0
            legacy = median(samples, "parallelism.scheduler_realization_milli") / 1000.0
            lines.append(
                f"{mode:16s} {contention:4s} candidate={candidate:.0f} pre={pre:.0f} "
                f"scheduled={scheduled:.0f} elided={elided:.0f} "
                f"dep_compression={ratio(pre, scheduled):.1f}x feedback_raw={raw_feedback:.0f} "
                f"feedback_batches={feedback_batches:.0f} feedback_batching={ratio(raw_feedback, feedback_batches):.1f}x "
                f"serialization={serialization_obs:.0f}/{serialization_batches:.0f} "
                f"feedback={fmt_ms(feedback_ns)} speedup={ratio(serial, wall):.2f}x "
                f"scheduler={corrected:.3f}x legacy={legacy:.3f}x"
            )

    forced = [r for r in records if r.get("metadata", {}).get("experiment_id") == "conflictlab-forced-speculation"]
    if forced:
        lines += ["", "=== forced speculation / prediction quality ==="]
        groups = defaultdict(list)
        for record in forced:
            p = record["metadata"]["parameters"]
            groups[(
                p.get("prediction_quality", "?"),
                p.get("hot_account_probability_bps", "?"),
                p.get("warmup_blocks", "?"),
                record["metadata"]["mode"],
            )].append(record)
        for key in sorted(groups):
            quality, hot, warmup, mode = key
            samples = groups[key]
            soft = median(samples, "scheduling.soft_edges")
            hard = median(samples, "scheduling.hard_edges")
            replayed = median(samples, "execution.replayed_transactions")
            misses = median(samples, "feedback.candidate_misses")
            replay_impact = median(samples, "feedback.replay_impact_observations")
            feedback_ns = median(samples, "feedback_timing.total_nanos")
            wall = median(samples, "parallelism.actual_execution_wall_nanos")
            serial = median(samples, "parallelism.serial_equivalent_work_nanos")
            lines.append(
                f"{quality:6s} hot={int(hot)/100:.0f}% warmup={warmup:>2s} {mode:16s} "
                f"soft={soft:.0f} hard={hard:.0f} replayed={replayed:.0f} misses={misses:.0f} "
                f"replay_evidence={replay_impact:.0f} feedback={fmt_ms(feedback_ns)} "
                f"speedup={ratio(serial, wall):.2f}x"
            )

    lines += ["", "Upload this summary plus records.jsonl and aggregate/summary-wide.csv for analysis."]
    text = "\n".join(lines) + "\n"
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
