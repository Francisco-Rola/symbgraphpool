#!/usr/bin/env python3
"""Summarize the controlled ConflictLab parallelism-ceiling experiment."""

from __future__ import annotations

import argparse
import csv
import json
import statistics
from collections import defaultdict
from pathlib import Path

EXPERIMENT_ID = "conflictlab-parallelism-ceiling"
WORKERS = 6


def load_records(path: Path) -> list[dict]:
    records: list[dict] = []
    with path.open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: {error}") from error
    if not records:
        raise ValueError("parallelism-ceiling records are empty")
    return records


def value(record: dict, *path: str, default=0):
    current = record
    for part in path:
        if not isinstance(current, dict):
            return default
        current = current.get(part)
        if current is None:
            return default
    return current


def median(values):
    values = list(values)
    return statistics.median(values) if values else 0.0


def ratio(numerator: float, denominator: float) -> float:
    return numerator / denominator if denominator else 0.0


def metric_row(records: list[dict]) -> dict[str, float | int | str]:
    sample = records[0]
    params = sample["metadata"]["parameters"]
    lanes = int(params["parallelism_lanes"])
    mode = sample["metadata"]["mode"]
    work_iterations = int(params["work_iterations"])
    complexity = params.get("complexity", str(work_iterations))

    serial = [float(value(r, "parallelism", "serial_equivalent_work_nanos")) for r in records]
    oracle = [float(value(r, "parallelism", "perfect_conflict_parallel_lower_bound_nanos")) for r in records]
    worker = [float(value(r, "parallelism", "actual_execution_wall_nanos")) for r in records]
    observed_bound = [float(value(r, "parallelism", "parallel_lower_bound_nanos")) for r in records]
    observed_work = [float(value(r, "parallelism", "observed_service_work_nanos")) for r in records]
    bottleneck = [float(value(r, "consensus", "bottleneck_nanos")) for r in records]
    sequential = [float(value(r, "pipeline_timing", "total_adaptive_block_nanos")) for r in records]

    oracle_speedups = [ratio(s, o) for s, o in zip(serial, oracle)]
    executor_speedups = [ratio(s, w) for s, w in zip(serial, worker)]
    phase_speedups = [ratio(s, b) for s, b in zip(serial, bottleneck)]
    sequential_speedups = [ratio(s, q) for s, q in zip(serial, sequential)]
    executor_eff = [100.0 * ratio(o, w) for o, w in zip(oracle, worker)]
    phase_eff = [100.0 * ratio(o, b) for o, b in zip(oracle, bottleneck)]
    sequential_eff = [100.0 * ratio(o, q) for o, q in zip(oracle, sequential)]

    planning = [float(value(r, "pipeline_timing", "planning_nanos")) for r in records]
    dep_setup = [float(value(r, "execution", "dependency_plan_setup_nanos")) for r in records]
    preexecution = [float(value(r, "pipeline_timing", "preexecution_nanos")) for r in records]
    pre_feedback = [float(value(r, "pipeline_timing", "pre_execution_feedback_nanos")) for r in records]
    reconciliation = [float(value(r, "pipeline_timing", "reconciliation_nanos")) for r in records]
    post_feedback = [float(value(r, "pipeline_timing", "reconciliation_feedback_nanos")) for r in records]
    pre_wrapper = [max(0.0, p - w) for p, w in zip(preexecution, worker)]
    executor_gap = [w - o for w, o in zip(worker, oracle)]
    oracle_to_observed_bound_gap = [b - o for b, o in zip(observed_bound, oracle)]
    scheduler_gap = [max(0.0, w - b) for w, b in zip(worker, observed_bound)]
    service_work_inflation = [100.0 * (ratio(w, s) - 1.0) for w, s in zip(observed_work, serial)]
    scheduler_overhead = [100.0 * (ratio(w, b) - 1.0) for w, b in zip(worker, observed_bound)]

    transactions = max(1, int(value(sample, "execution", "transactions", default=1)))
    us_per_tx = 1.0 / (transactions * 1000.0)

    contract_paths = {
        "ready_wait_us_tx": ("execution", "aggregate_ready_wait_nanos"),
        "visibility_us_tx": ("execution", "aggregate_visibility_capture_nanos"),
        "contract_us_tx": ("execution", "aggregate_contract_execution_nanos"),
        "publish_us_tx": ("execution", "aggregate_publish_and_unblock_nanos"),
        "wasm_acquire_us_tx": ("execution", "contract", "aggregate_wasm_instance_acquire_nanos"),
        "wasm_entrypoint_us_tx": ("execution", "contract", "aggregate_wasm_entrypoint_nanos"),
        "host_storage_us_tx": ("execution", "contract", "aggregate_host_storage_nanos"),
        "mvcc_point_us_tx": ("execution", "contract", "aggregate_mvcc_storage_point_nanos"),
        "mvcc_publish_us_tx": ("execution", "contract", "aggregate_mvcc_publish_nanos"),
    }

    row: dict[str, float | int | str] = {
        "mode": mode,
        "complexity": complexity,
        "work_iterations": work_iterations,
        "parallelism_lanes": lanes,
        "nominal_worker_ceiling_x": min(lanes, WORKERS),
        "records": len(records),
        "oracle_speedup_x": median(oracle_speedups),
        "executor_speedup_x": median(executor_speedups),
        "executor_oracle_efficiency_pct": median(executor_eff),
        "phase_speedup_x": median(phase_speedups),
        "phase_oracle_efficiency_pct": median(phase_eff),
        "sequential_speedup_x": median(sequential_speedups),
        "sequential_oracle_efficiency_pct": median(sequential_eff),
        "max_in_flight": median(value(r, "execution", "max_in_flight") for r in records),
        "serial_ms": median(serial) / 1e6,
        "oracle_bound_ms": median(oracle) / 1e6,
        "worker_wall_ms": median(worker) / 1e6,
        "observed_parallel_lower_bound_ms": median(observed_bound) / 1e6,
        "executor_gap_ms": median(executor_gap) / 1e6,
        "oracle_to_observed_bound_gap_ms": median(oracle_to_observed_bound_gap) / 1e6,
        "scheduler_gap_ms": median(scheduler_gap) / 1e6,
        "service_work_inflation_pct": median(service_work_inflation),
        "scheduler_overhead_pct": median(scheduler_overhead),
        "planning_ms": median(planning) / 1e6,
        "dependency_setup_ms": median(dep_setup) / 1e6,
        "preexecution_wrapper_ms": median(pre_wrapper) / 1e6,
        "pre_feedback_ms": median(pre_feedback) / 1e6,
        "reconciliation_ms": median(reconciliation) / 1e6,
        "post_feedback_ms": median(post_feedback) / 1e6,
        "adaptive_total_ms": median(sequential) / 1e6,
    }
    for name, path in contract_paths.items():
        row[name] = median(float(value(r, *path)) for r in records) * us_per_tx
    return row


def format_float(value_: float, digits: int = 2) -> str:
    return f"{value_:.{digits}f}"


def render_table(rows: list[dict], columns: list[tuple[str, str, int]]) -> list[str]:
    rendered = []
    header = "  ".join(label.rjust(width) for _, label, width in columns)
    rendered.append(header)
    rendered.append("  ".join(("-" * width) for _, _, width in columns))
    for row in rows:
        cells = []
        for key, _, width in columns:
            item = row[key]
            if isinstance(item, float):
                text = format_float(item)
            else:
                text = str(item)
            cells.append(text.rjust(width))
        rendered.append("  ".join(cells))
    return rendered


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--csv", type=Path)
    args = parser.parse_args()

    records = load_records(args.records)
    errors = []
    for record in records:
        metadata = record.get("metadata", {})
        if metadata.get("experiment_id") != EXPERIMENT_ID:
            errors.append(f"unexpected experiment_id={metadata.get('experiment_id')!r}")
        if metadata.get("workers") != WORKERS:
            errors.append(f"workers={metadata.get('workers')!r}, expected {WORKERS}")
        params = metadata.get("parameters", {})
        if int(params.get("parallelism_lanes", "0")) <= 0:
            errors.append("parallelism_lanes must be positive")
        if params.get("prediction_quality") != "exact":
            errors.append(f"run_index={metadata.get('run_index')} prediction_quality must be exact")
        if params.get("consensus_divergence") != "identical":
            errors.append(f"run_index={metadata.get('run_index')} consensus_divergence must be identical")
        if record.get("correctness", {}).get("serial_equivalent") is not True:
            errors.append(f"run_index={metadata.get('run_index')} is not serial-equivalent")
        consensus = record.get("consensus", {})
        if consensus.get("cutoff_reached") is not False:
            errors.append(f"run_index={metadata.get('run_index')} reached the consensus cutoff")
        if consensus.get("prepared_receipts") != consensus.get("candidate_transactions"):
            errors.append(f"run_index={metadata.get('run_index')} did not complete preexecution")
        required_paths = [
            ("parallelism", "serial_equivalent_work_nanos"),
            ("parallelism", "perfect_conflict_parallel_lower_bound_nanos"),
            ("parallelism", "observed_service_work_nanos"),
            ("parallelism", "parallel_lower_bound_nanos"),
            ("parallelism", "actual_execution_wall_nanos"),
            ("consensus", "bottleneck_nanos"),
            ("execution", "transactions"),
            ("execution", "max_in_flight"),
            ("execution", "dependency_plan_setup_nanos"),
            ("pipeline_timing", "planning_nanos"),
            ("pipeline_timing", "preexecution_nanos"),
            ("pipeline_timing", "pre_execution_feedback_nanos"),
            ("pipeline_timing", "reconciliation_nanos"),
            ("pipeline_timing", "reconciliation_feedback_nanos"),
            ("pipeline_timing", "total_adaptive_block_nanos"),
        ]
        for section, field in required_paths:
            if record.get(section, {}).get(field) is None:
                errors.append(f"run_index={metadata.get('run_index')} missing {section}.{field}")
    if errors:
        raise SystemExit("parallelism-ceiling validation failed: " + "; ".join(errors[:8]))

    grouped: dict[tuple[str, int, int], list[dict]] = defaultdict(list)
    for record in records:
        metadata = record["metadata"]
        params = metadata["parameters"]
        grouped[(metadata["mode"], int(params["work_iterations"]), int(params["parallelism_lanes"]))].append(record)
    rows = [metric_row(group) for _, group in sorted(grouped.items())]

    lines = [
        "ConflictLab controlled parallelism ceiling",
        f"records={len(records)} groups={len(rows)} workers={WORKERS}",
        "",
        "Lane sweep at high compute (786432 iterations)",
        "Nominal = min(controlled conflict lanes, 6 workers). Oracle uses measured serial service costs + concrete conflicts.",
    ]
    lane_rows = sorted(
        (r for r in rows if r["work_iterations"] == 786432),
        key=lambda r: (r["mode"], r["parallelism_lanes"]),
    )
    lines.extend(
        render_table(
            lane_rows,
            [
                ("mode", "mode", 16),
                ("parallelism_lanes", "lanes", 6),
                ("nominal_worker_ceiling_x", "nominal", 8),
                ("oracle_speedup_x", "oracle", 8),
                ("executor_speedup_x", "executor", 9),
                ("executor_oracle_efficiency_pct", "exec%", 7),
                ("phase_speedup_x", "phase", 7),
                ("phase_oracle_efficiency_pct", "phase%", 7),
                ("sequential_speedup_x", "seq", 7),
                ("max_in_flight", "inflight", 8),
            ],
        )
    )

    lines.extend(["", "Overhead amortization at the 6x hardware ceiling", ""])
    ceiling_rows = sorted(
        (r for r in rows if r["parallelism_lanes"] in {6, 384}),
        key=lambda r: (r["mode"], r["parallelism_lanes"], r["work_iterations"]),
    )
    lines.extend(
        render_table(
            ceiling_rows,
            [
                ("mode", "mode", 16),
                ("parallelism_lanes", "lanes", 6),
                ("work_iterations", "work", 8),
                ("oracle_speedup_x", "oracle", 8),
                ("executor_speedup_x", "executor", 9),
                ("phase_speedup_x", "phase", 7),
                ("sequential_speedup_x", "seq", 7),
                ("executor_gap_ms", "exec-gap", 9),
                ("planning_ms", "plan-ms", 8),
                ("reconciliation_ms", "recon-ms", 9),
            ],
        )
    )

    lines.extend([
        "",
        "Wall-clock overhead breakdown for ceiling cases",
        "bound-gap = observed-service/scheduled-DAG lower bound - hindsight oracle; sched-gap = worker wall - observed lower bound.",
        "Together they explain the executor gap; pipeline stages after worker execution are shown separately.",
    ])
    lines.extend(
        render_table(
            ceiling_rows,
            [
                ("mode", "mode", 16),
                ("parallelism_lanes", "lanes", 6),
                ("work_iterations", "work", 8),
                ("oracle_bound_ms", "oracle-ms", 9),
                ("observed_parallel_lower_bound_ms", "obs-lb-ms", 9),
                ("worker_wall_ms", "worker-ms", 9),
                ("oracle_to_observed_bound_gap_ms", "bound-gap", 9),
                ("scheduler_gap_ms", "sched-gap", 9),
                ("planning_ms", "plan", 7),
                ("preexecution_wrapper_ms", "pre-wrap", 8),
                ("pre_feedback_ms", "pre-fb", 7),
                ("reconciliation_ms", "recon", 7),
                ("post_feedback_ms", "post-fb", 7),
            ],
        )
    )

    lines.extend([
        "",
        "Executor loss decomposition (percent)",
        "service-work inflation compares aggregate observed parallel service work with serial work; scheduler overhead is worker wall over the observed feasible lower bound.",
    ])
    lines.extend(
        render_table(
            ceiling_rows,
            [
                ("mode", "mode", 16),
                ("parallelism_lanes", "lanes", 6),
                ("work_iterations", "work", 8),
                ("service_work_inflation_pct", "svc-infl%", 10),
                ("scheduler_overhead_pct", "sched-ovh%", 10),
                ("executor_oracle_efficiency_pct", "exec%", 8),
                ("phase_oracle_efficiency_pct", "phase%", 8),
                ("sequential_oracle_efficiency_pct", "seq%", 8),
            ],
        )
    )

    lines.extend([
        "",
        "Aggregate worker hot-path cost (microseconds/tx; nested diagnostics are not additive)",
    ])
    lines.extend(
        render_table(
            ceiling_rows,
            [
                ("mode", "mode", 16),
                ("parallelism_lanes", "lanes", 6),
                ("work_iterations", "work", 8),
                ("contract_us_tx", "contract", 9),
                ("wasm_entrypoint_us_tx", "wasm", 8),
                ("wasm_acquire_us_tx", "acquire", 8),
                ("host_storage_us_tx", "host-st", 8),
                ("mvcc_point_us_tx", "mvcc-rd", 8),
                ("mvcc_publish_us_tx", "mvcc-pub", 9),
                ("visibility_us_tx", "vis", 7),
                ("publish_us_tx", "publish", 8),
            ],
        )
    )

    best = max(rows, key=lambda r: (r["phase_speedup_x"], r["executor_speedup_x"]))
    lines.extend([
        "",
        "Best measured phase result",
        (
            f"mode={best['mode']} lanes={best['parallelism_lanes']} work={best['work_iterations']} "
            f"oracle={best['oracle_speedup_x']:.2f}x executor={best['executor_speedup_x']:.2f}x "
            f"phase={best['phase_speedup_x']:.2f}x sequential={best['sequential_speedup_x']:.2f}x "
            f"executor_oracle_efficiency={best['executor_oracle_efficiency_pct']:.1f}%"
        ),
    ])

    text = "\n".join(lines) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    else:
        print(text, end="")

    if args.csv:
        args.csv.parent.mkdir(parents=True, exist_ok=True)
        fieldnames = list(rows[0].keys())
        with args.csv.open("w", encoding="utf-8", newline="") as handle:
            writer = csv.DictWriter(handle, fieldnames=fieldnames)
            writer.writeheader()
            writer.writerows(rows)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
