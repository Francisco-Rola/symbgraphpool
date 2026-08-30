#!/usr/bin/env python3
"""Flatten accepted ExperimentRecord JSONL and emit grouped/plot-ready statistics."""

from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from pathlib import Path

from consensus_pipeline_metrics import consensus_pipeline_metrics
from conflictlab_v1_miss_policy import (
    candidate_misses,
    classify_candidate_miss,
    has_recovery_evidence,
)

METRICS = {
    "parallel_wall_ms": ("parallelism.actual_execution_wall_nanos", 1e-6),
    "serial_work_ms": ("parallelism.serial_equivalent_work_nanos", 1e-6),
    "serial_dag_ms": ("parallelism.serial_cost_dag_bound_nanos", 1e-6),
    "perfect_conflict_dag_ms": ("parallelism.perfect_conflict_dag_bound_nanos", 1e-6),
    "perfect_conflict_lower_bound_ms": ("parallelism.perfect_conflict_parallel_lower_bound_nanos", 1e-6),
    "perfect_conflict_oracle_speedup": ("derived.perfect_conflict_oracle_speedup", 1.0),
    "oracle_realization": ("derived.oracle_realization", 1.0),
    "observed_dag_ms": ("parallelism.observed_service_dag_bound_nanos", 1e-6),
    "observed_service_work_ms": ("parallelism.observed_service_work_nanos", 1e-6),
    "worker_capacity_bound_ms": ("parallelism.worker_capacity_bound_nanos", 1e-6),
    "parallel_lower_bound_ms": ("parallelism.parallel_lower_bound_nanos", 1e-6),
    "speedup": ("derived.parallel_speedup", 1.0),
    "service_inflation": ("parallelism.service_inflation_milli", 1e-3),
    "scheduler_realization_legacy": ("parallelism.scheduler_realization_milli", 1e-3),
    "scheduler_realization": ("parallelism.scheduler_realization_corrected_milli", 1e-3),
    "planning_ms": ("planning.total_nanos", 1e-6),
    "serial_bypass": ("planning.serial_bypassed", 1.0),
    "serial_bypass_projected_speedup": ("planning.serial_bypass_projected_speedup_milli", 1e-3),
    "serial_bypass_mean_service_us": ("planning.serial_bypass_mean_service_nanos", 1e-3),
    "serial_bypass_admission_score": ("planning.serial_bypass_admission_score_milli", 1e-3),
    "feedback_ms": ("feedback_timing.total_nanos", 1e-6),
    "adaptive_static_relationships": ("adaptive_state.static_relationships", 1.0),
    "adaptive_runtime_fallback_relationships": ("adaptive_state.runtime_fallback_relationships", 1.0),
    "adaptive_miss_history_relationships": ("adaptive_state.candidate_miss_history_relationships", 1.0),
    "adaptive_mean_probability": ("adaptive_state.mean_probability_q16", 1.0 / 65535.0),
    "adaptive_mean_confidence": ("adaptive_state.mean_confidence_q16", 1.0 / 65535.0),
    "prediction_precision": ("derived.prediction_precision", 1.0),
    "prediction_recall": ("derived.prediction_recall", 1.0),
    "pipeline_total_ms": ("pipeline_timing.total_adaptive_block_nanos", 1e-6),
    "pipeline_planning_ms": ("pipeline_timing.planning_nanos", 1e-6),
    "pipeline_preexecution_ms": ("pipeline_timing.preexecution_nanos", 1e-6),
    "pipeline_reconciliation_ms": ("pipeline_timing.reconciliation_nanos", 1e-6),
    "pipeline_speedup": ("pipeline_timing.end_to_end_speedup_milli", 1e-3),
    "matched_serial_speedup": ("derived.matched_serial_speedup", 1.0),
    "matched_serial_total_ms": ("derived.matched_serial_total_nanos", 1e-6),
    "total_work_speedup": ("derived.total_work_speedup", 1.0),
    "preconsensus_eligible_ms": ("derived.preconsensus_eligible_nanos", 1e-6),
    "pre_consensus_ms": ("derived.pre_consensus_nanos", 1e-6),
    "preconsensus_spill_ms": ("derived.preconsensus_spill_nanos", 1e-6),
    "intrinsic_postconsensus_ms": ("derived.intrinsic_postconsensus_nanos", 1e-6),
    "post_consensus_ms": ("derived.post_consensus_nanos", 1e-6),
    "consensus_bottleneck_ms": ("derived.consensus_bottleneck_nanos", 1e-6),
    "preexecution_complete_before_consensus": ("derived.preexecution_complete_before_consensus", 1.0),
    "serial_validation_latency_ms": ("derived.serial_validation_latency_nanos", 1e-6),
    "acg_validation_latency_ms": ("derived.acg_validation_latency_nanos", 1e-6),
    "validation_latency_speedup": ("derived.validation_latency_speedup", 1.0),
    "serial_throughput_blocks_per_s": ("derived.serial_throughput_blocks_per_s", 1.0),
    "acg_throughput_blocks_per_s": ("derived.acg_throughput_blocks_per_s", 1.0),
    "serial_throughput_tps": ("derived.serial_throughput_tps", 1.0),
    "acg_throughput_tps": ("derived.acg_throughput_tps", 1.0),
    "throughput_speedup": ("derived.throughput_speedup", 1.0),
    "feedback_us_per_observation": ("derived.feedback_nanos_per_observation", 1e-3),
    "replay_ms": ("execution.replay_or_missing_execution_nanos", 1e-6),
    "replayed_transactions": ("execution.replayed_transactions", 1.0),
    "invalidated_results": ("execution.invalidated_results", 1.0),
    "reused_results": ("execution.reused_results", 1.0),
    "successful_preexecution_receipts": ("consensus.successful_preexecution_receipts", 1.0),
    "failed_preexecution_receipts": ("consensus.failed_preexecution_receipts", 1.0),
    "candidate_edges": ("scheduling.candidate_edges", 1.0),
    "materialized_candidate_edges": ("scheduling.materialized_candidate_edges", 1.0),
    "candidate_materialization_compression": ("derived.candidate_materialization_compression", 1.0),
    "pre_reduction_dependencies": ("scheduling.pre_reduction_dependencies", 1.0),
    "scheduled_dependencies": ("scheduling.scheduled_dependencies", 1.0),
    "edges_elided_by_reduction": ("scheduling.edges_elided_by_reduction", 1.0),
    "dependency_compression": ("derived.dependency_compression", 1.0),
    "low_edges": ("scheduling.low_edges", 1.0),
    "soft_edges": ("scheduling.soft_edges", 1.0),
    "hard_edges": ("scheduling.hard_edges", 1.0),
    "wave_count": ("scheduling.wave_count", 1.0),
    "soft_dependencies": ("scheduling.soft_dependencies", 1.0),
    # Scheduling hard dependencies describe the canonical/replay dependency plan. This is
    # identical to execution.hard_dependency_count for single-plan strategies, but Vegeta-like
    # intentionally has a fully-parallel discovery pass followed by a separate replay DAG.
    "hard_dependencies": ("scheduling.hard_dependencies", 1.0),
    "max_in_flight": ("execution.max_in_flight", 1.0),
    "strategy_discovery_transactions": ("strategy.discovery_transactions", 1.0),
    "strategy_discovered_conflicts": ("strategy.discovered_conflicts", 1.0),
    "strategy_forward_conflict_fallbacks": ("strategy.forward_conflict_fallbacks", 1.0),
    "strategy_access_set_mismatch_fallbacks": ("strategy.access_set_mismatch_fallbacks", 1.0),
    "strategy_replay_dependencies": ("strategy.replay_dependencies", 1.0),
    "strategy_replay_parallel_ms": ("strategy.replay_parallel_nanos", 1e-6),
    "wasm_acquire_ms": ("execution.contract.aggregate_wasm_instance_acquire_nanos", 1e-6),
    "wasm_instance_reuse_hits": ("execution.contract.wasm_instance_reuse_hits", 1.0),
    "wasm_instance_pool_misses": ("execution.contract.wasm_instance_pool_misses", 1.0),
    "wasm_entrypoint_ms": ("execution.contract.aggregate_wasm_entrypoint_nanos", 1e-6),
    "wasm_recycle_ms": ("execution.contract.aggregate_wasm_recycle_nanos", 1e-6),
    "wasm_lifecycle_us_per_tx": ("derived.wasm_lifecycle_nanos_per_tx", 1e-3),
    "wasm_lifecycle_share": ("derived.wasm_lifecycle_share", 1.0),
    "host_storage_ms": ("execution.contract.aggregate_host_storage_nanos", 1e-6),
    "mvcc_point_ms": ("execution.contract.aggregate_mvcc_storage_point_nanos", 1e-6),
    "mvcc_range_ms": ("execution.contract.aggregate_mvcc_storage_range_nanos", 1e-6),
    "positive_observations": ("feedback.positive_observations", 1.0),
    "negative_observations": ("feedback.negative_observations", 1.0),
    "candidate_misses": ("feedback.candidate_misses", 1.0),
    "feedback_batches": ("feedback.observation_batches_applied", 1.0),
    "feedback_batching_factor": ("derived.feedback_batching_factor", 1.0),
    "serialization_observations": ("feedback.serialization_cost_observations", 1.0),
    "serialization_batches": ("feedback.serialization_cost_batches_applied", 1.0),
    "serialization_batching_factor": ("derived.serialization_batching_factor", 1.0),
    "replay_impact_observations": ("feedback.replay_impact_observations", 1.0),
}


def flatten(value, prefix="", output=None):
    if output is None:
        output = {}
    if isinstance(value, dict):
        for key, child in value.items():
            child_prefix = f"{prefix}.{key}" if prefix else str(key)
            flatten(child, child_prefix, output)
    elif isinstance(value, list):
        output[prefix] = json.dumps(value, sort_keys=True, separators=(",", ":"))
    else:
        output[prefix] = value
    return output


def load_records(paths):
    records = []
    for path in paths:
        with path.open("r", encoding="utf-8") as handle:
            for line_number, line in enumerate(handle, start=1):
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError as error:
                    raise ValueError(f"{path}:{line_number}: {error}") from error
    return records


def derived_flat(record, preconsensus_window_ms=None):
    flat = flatten(record)
    wall = flat.get("parallelism.actual_execution_wall_nanos")
    serial = flat.get("parallelism.serial_equivalent_work_nanos")
    flat["derived.parallel_speedup"] = (
        float(serial) / float(wall) if wall not in (None, 0) and serial is not None else None
    )
    oracle_bound = flat.get("parallelism.perfect_conflict_parallel_lower_bound_nanos")
    flat["derived.perfect_conflict_oracle_speedup"] = (
        float(serial) / float(oracle_bound)
        if oracle_bound not in (None, 0) and serial is not None
        else None
    )
    flat["derived.oracle_realization"] = (
        float(wall) / float(oracle_bound)
        if oracle_bound not in (None, 0) and wall is not None
        else None
    )
    positives = flat.get("feedback.positive_observations")
    negatives = flat.get("feedback.negative_observations")
    misses = flat.get("feedback.candidate_misses")
    precision_denominator = (positives or 0) + (negatives or 0)
    recall_denominator = (positives or 0) + (misses or 0)
    flat["derived.prediction_precision"] = (
        float(positives) / float(precision_denominator)
        if positives is not None and precision_denominator > 0
        else None
    )
    flat["derived.prediction_recall"] = (
        float(positives) / float(recall_denominator)
        if positives is not None and recall_denominator > 0
        else None
    )
    logical_candidates = flat.get("scheduling.candidate_edges")
    materialized_candidates = flat.get("scheduling.materialized_candidate_edges")
    flat["derived.candidate_materialization_compression"] = (
        float(logical_candidates) / float(materialized_candidates)
        if materialized_candidates not in (None, 0) and logical_candidates is not None
        else None
    )
    pre_dependencies = flat.get("scheduling.pre_reduction_dependencies")
    scheduled_dependencies = flat.get("scheduling.scheduled_dependencies")
    flat["derived.dependency_compression"] = (
        float(pre_dependencies) / float(scheduled_dependencies)
        if scheduled_dependencies not in (None, 0) and pre_dependencies is not None
        else None
    )
    raw_feedback = (flat.get("feedback.positive_observations") or 0) + (
        flat.get("feedback.negative_observations") or 0
    )
    feedback_batches = flat.get("feedback.observation_batches_applied")
    flat["derived.feedback_batching_factor"] = (
        float(raw_feedback) / float(feedback_batches)
        if feedback_batches not in (None, 0)
        else None
    )
    feedback_nanos = flat.get("feedback_timing.total_nanos")
    flat["derived.feedback_nanos_per_observation"] = (
        float(feedback_nanos) / float(raw_feedback)
        if raw_feedback and feedback_nanos is not None
        else None
    )
    wasm_lifecycle_nanos = (flat.get("execution.contract.aggregate_wasm_instance_acquire_nanos") or 0) + (
        flat.get("execution.contract.aggregate_wasm_recycle_nanos") or 0
    )
    transactions = flat.get("execution.transactions")
    request_execution_nanos = flat.get("execution.contract.aggregate_request_execution_nanos")
    flat["derived.wasm_lifecycle_nanos_per_tx"] = (
        float(wasm_lifecycle_nanos) / float(transactions)
        if transactions not in (None, 0)
        else None
    )
    flat["derived.wasm_lifecycle_share"] = (
        float(wasm_lifecycle_nanos) / float(request_execution_nanos)
        if request_execution_nanos not in (None, 0)
        else None
    )
    consensus = consensus_pipeline_metrics(record, preconsensus_window_ms)
    flat["derived.preconsensus_window_nanos"] = consensus["preconsensus_window_nanos"]
    flat["derived.preconsensus_eligible_nanos"] = consensus["preconsensus_eligible_nanos"]
    flat["derived.pre_consensus_nanos"] = consensus["preconsensus_completed_nanos"]
    flat["derived.preconsensus_spill_nanos"] = consensus["preconsensus_spill_nanos"]
    flat["derived.intrinsic_postconsensus_nanos"] = consensus["intrinsic_postconsensus_nanos"]
    flat["derived.post_consensus_nanos"] = consensus["postconsensus_validation_nanos"]
    flat["derived.consensus_bottleneck_nanos"] = consensus["pipeline_bottleneck_nanos"]
    flat["derived.preexecution_complete_before_consensus"] = consensus[
        "preexecution_complete_before_consensus"
    ]
    flat["derived.serial_validation_latency_nanos"] = consensus["serial_validation_latency_nanos"]
    flat["derived.acg_validation_latency_nanos"] = consensus["postconsensus_validation_nanos"]
    validation_speedup = consensus["validation_latency_speedup"]
    flat["derived.validation_latency_speedup"] = (
        validation_speedup if math.isfinite(validation_speedup) else None
    )
    flat["derived.throughput_speedup"] = consensus["throughput_speedup"]
    flat["derived.serial_throughput_blocks_per_s"] = consensus["serial_throughput_blocks_per_s"]
    flat["derived.acg_throughput_blocks_per_s"] = consensus["acg_throughput_blocks_per_s"]
    flat["derived.serial_throughput_tps"] = consensus["serial_throughput_tps"]
    flat["derived.acg_throughput_tps"] = consensus["acg_throughput_tps"]
    serial_reference_nanos = flat.get("pipeline_timing.serial_reference_execution_nanos")
    total_adaptive_nanos = flat.get("pipeline_timing.total_adaptive_block_nanos")
    flat["derived.total_work_speedup"] = (
        float(serial_reference_nanos) / float(total_adaptive_nanos)
        if serial_reference_nanos is not None and total_adaptive_nanos not in (None, 0)
        else None
    )
    serialization_observations = flat.get("feedback.serialization_cost_observations")
    serialization_batches = flat.get("feedback.serialization_cost_batches_applied")
    flat["derived.serialization_batching_factor"] = (
        float(serialization_observations) / float(serialization_batches)
        if serialization_batches not in (None, 0) and serialization_observations is not None
        else None
    )
    classification = classify_candidate_miss(record)
    flat["derived.candidate_miss_class"] = classification or "none"
    flat["derived.candidate_miss_recovery_evidence"] = (
        has_recovery_evidence(record) if candidate_misses(record) > 0 else None
    )
    params = record.get("metadata", {}).get("parameters", {})
    for key, value in params.items():
        flat[f"param.{key}"] = value
    return flat


def percentile(values, probability):
    ordered = sorted(values)
    if not ordered:
        return None
    if len(ordered) == 1:
        return ordered[0]
    position = probability * (len(ordered) - 1)
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    fraction = position - lower
    return ordered[lower] * (1.0 - fraction) + ordered[upper] * fraction


def describe(values):
    n = len(values)
    mean = statistics.fmean(values)
    median = statistics.median(values)
    stdev = statistics.stdev(values) if n > 1 else 0.0
    half_width = 1.96 * stdev / math.sqrt(n) if n > 1 else 0.0
    return {
        "n": n,
        "mean": mean,
        "median": median,
        "stdev": stdev,
        "min": min(values),
        "p05": percentile(values, 0.05),
        "p95": percentile(values, 0.95),
        "max": max(values),
        "ci95_low": mean - half_width,
        "ci95_high": mean + half_width,
    }


def matched_serial_key(record):
    metadata = record.get("metadata", {})
    params = metadata.get("parameters", {})
    return (
        metadata.get("experiment_id"),
        metadata.get("workload"),
        metadata.get("workers"),
        metadata.get("seed"),
        tuple(sorted((str(k), str(v)) for k, v in params.items())),
    )


def add_matched_serial_metrics(records, flat_records):
    serial_walls = {}
    for record in records:
        if record.get("metadata", {}).get("mode") != "serial":
            continue
        wall = record.get("pipeline_timing", {}).get("total_adaptive_block_nanos")
        if isinstance(wall, (int, float)) and wall > 0:
            serial_walls.setdefault(matched_serial_key(record), []).append(float(wall))

    for record, flat in zip(records, flat_records):
        candidates = serial_walls.get(matched_serial_key(record), [])
        if not candidates:
            flat["derived.matched_serial_total_nanos"] = None
            flat["derived.matched_serial_speedup"] = None
            continue
        serial_wall = statistics.median(candidates)
        adaptive_wall = record.get("pipeline_timing", {}).get("total_adaptive_block_nanos")
        flat["derived.matched_serial_total_nanos"] = serial_wall
        flat["derived.matched_serial_speedup"] = (
            serial_wall / float(adaptive_wall)
            if isinstance(adaptive_wall, (int, float)) and adaptive_wall > 0
            else None
        )


def group_key(record):
    metadata = record.get("metadata", {})
    params = metadata.get("parameters", {})
    return (
        metadata.get("experiment_id"),
        metadata.get("workload"),
        metadata.get("mode"),
        metadata.get("workers"),
        tuple(sorted((str(k), str(v)) for k, v in params.items())),
    )


def write_csv(path, rows, fieldnames):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path, nargs="+")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument(
        "--preconsensus-window-ms",
        type=float,
        help="optional pre-consensus execution budget; omitted means all eligible pre-execution completes",
    )
    args = parser.parse_args()

    records = load_records(args.records)
    if not records:
        raise SystemExit("no records found")
    flat_records = [derived_flat(record, args.preconsensus_window_ms) for record in records]
    add_matched_serial_metrics(records, flat_records)
    all_fields = sorted({key for record in flat_records for key in record})
    write_csv(args.out_dir / "records-flat.csv", flat_records, all_fields)

    grouped = {}
    for record, flat in zip(records, flat_records):
        grouped.setdefault(group_key(record), []).append(flat)

    parameter_keys = sorted(
        {key for record in flat_records for key in record if key.startswith("param.")}
    )
    plot_rows = []
    summary_rows = []
    for key, samples in sorted(grouped.items(), key=lambda item: repr(item[0])):
        experiment_id, workload, mode, workers, parameters = key
        base = {
            "experiment_id": experiment_id,
            "workload": workload,
            "mode": mode,
            "workers": workers,
        }
        base.update({f"param.{name}": value for name, value in parameters})
        wide = dict(base)
        wide["n"] = len(samples)
        for metric, (path, scale) in METRICS.items():
            values = []
            for sample in samples:
                value = sample.get(path)
                if isinstance(value, (int, float)) and value is not None:
                    values.append(float(value) * scale)
            if not values:
                continue
            stats = describe(values)
            for stat, value in stats.items():
                wide[f"{metric}.{stat}"] = value
            plot_rows.append({**base, "metric": metric, **stats})
        summary_rows.append(wide)

    summary_fields = sorted({key for row in summary_rows for key in row})
    plot_fields = ["experiment_id", "workload", "mode", "workers", *parameter_keys, "metric", "n", "mean", "median", "stdev", "min", "p05", "p95", "max", "ci95_low", "ci95_high"]
    write_csv(args.out_dir / "summary-wide.csv", summary_rows, summary_fields)
    write_csv(args.out_dir / "plot-long.csv", plot_rows, plot_fields)

    miss_groups = {}
    for record in records:
        misses = candidate_misses(record)
        if misses <= 0:
            continue
        metadata = record.get("metadata", {})
        operation_mix = metadata.get("parameters", {}).get("operation_mix", "n/a")
        key = (
            classify_candidate_miss(record),
            metadata.get("experiment_id"),
            operation_mix,
        )
        row = miss_groups.setdefault(
            key,
            {
                "classification": key[0],
                "experiment_id": key[1],
                "operation_mix": key[2],
                "records_with_misses": 0,
                "candidate_misses": 0,
                "records_with_recovery_evidence": 0,
            },
        )
        row["records_with_misses"] += 1
        row["candidate_misses"] += misses
        row["records_with_recovery_evidence"] += int(has_recovery_evidence(record))
    write_csv(
        args.out_dir / "candidate-miss-attribution.csv",
        [miss_groups[key] for key in sorted(miss_groups, key=repr)],
        [
            "classification",
            "experiment_id",
            "operation_mix",
            "records_with_misses",
            "candidate_misses",
            "records_with_recovery_evidence",
        ],
    )
    (args.out_dir / "summary.json").write_text(
        json.dumps({"records": len(records), "groups": len(grouped), "metrics": list(METRICS)}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"aggregated {len(records)} records into {len(grouped)} groups -> {args.out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
