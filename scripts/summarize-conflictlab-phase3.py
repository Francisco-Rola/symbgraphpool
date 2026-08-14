#!/usr/bin/env python3
"""Human-readable Phase 3 ConflictLab summary with control-plane and Wasm lifecycle metrics."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def f(value: object, default: float = 0.0) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def i(value: object, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def record_key(record: dict) -> tuple:
    p = record["metadata"]["parameters"]
    return (
        i(p.get("sim.block_size")),
        p.get("complexity", "?"),
        p.get("prediction_quality", "?"),
        p.get("contention", "?"),
        p.get("acg.serial_bypass_enabled", "?"),
        f(p.get("acg.risk_budget")),
        f(p.get("acg.exploration_rate")),
        record["metadata"].get("mode", "?"),
    )


def describe(record: dict) -> str:
    p = record["metadata"]["parameters"]
    planning = record.get("planning", {})
    sched = record.get("scheduling", {})
    execution = record.get("execution", {})
    feedback = record.get("feedback", {})
    feedback_timing = record.get("feedback_timing", {})
    pipeline = record.get("pipeline_timing", {})
    contract = execution.get("contract", {})

    n = max(i(execution.get("transactions")), 1)
    pre = i(sched.get("pre_reduction_dependencies"))
    final = i(sched.get("scheduled_dependencies"))
    compression = pre / max(final, 1) if pre else 0.0
    lifecycle_ns = i(contract.get("aggregate_wasm_instance_acquire_nanos")) + i(
        contract.get("aggregate_wasm_recycle_nanos")
    )
    request_ns = max(i(contract.get("aggregate_request_execution_nanos")), 1)
    lifecycle_share = lifecycle_ns / request_ns
    pipeline_speedup = i(pipeline.get("end_to_end_speedup_milli")) / 1000.0
    serial_ms = i(pipeline.get("serial_reference_execution_nanos")) / 1e6
    total_ms = i(pipeline.get("total_adaptive_block_nanos")) / 1e6
    feedback_ms = i(feedback_timing.get("total_nanos")) / 1e6
    bypassed = bool(planning.get("serial_bypassed", False))
    bypass_projection = planning.get("serial_bypass_projected_speedup_milli")
    projection = "-" if bypass_projection is None else f"{i(bypass_projection) / 1000.0:.2f}x"

    experiment = record["metadata"].get("experiment_id", "?").replace("conflictlab-phase3-", "")
    seed = i(record["metadata"].get("seed"))
    return (
        f"exp={experiment:<11} seed={seed:<3} B{i(p.get('sim.block_size')):<3} {p.get('complexity','?'):<6} "
        f"pred={p.get('prediction_quality','?'):<8} cont={p.get('contention','?'):<5} "
        f"bypass_cfg={p.get('acg.serial_bypass_enabled','?'):<5} risk={f(p.get('acg.risk_budget')):.2f} "
        f"explore={f(p.get('acg.exploration_rate')):.2f} {record['metadata'].get('mode','?'):<16} "
        f"bypassed={str(bypassed):<5} proj={projection:<5} "
        f"low/soft/hard={i(sched.get('low_edges'))}/{i(sched.get('soft_edges'))}/{i(sched.get('hard_edges'))} "
        f"pre/final={pre}/{final} comp={compression:.1f}x waves={i(sched.get('wave_count'))} "
        f"replay={i(execution.get('replayed_transactions'))} "
        f"serObs/batch={i(feedback.get('serialization_cost_observations'))}/{i(feedback.get('serialization_cost_batches_applied'))} "
        f"feedback={feedback_ms:.3f}ms "
        f"vmLife={lifecycle_ns / 1e6:.3f}ms ({lifecycle_ns / n / 1e3:.1f}us/tx,{lifecycle_share*100:.1f}%) "
        f"pipeline={pipeline_speedup:.2f}x total={total_ms:.3f}ms serial={serial_ms:.3f}ms"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records")
    parser.add_argument("--output")
    args = parser.parse_args()

    records = [json.loads(line) for line in Path(args.records).read_text().splitlines() if line.strip()]
    records.sort(key=record_key)
    if not records:
        raise SystemExit("no records")

    schema_versions = sorted({i(r.get("schema_version")) for r in records})
    complexities = sorted({r["metadata"]["parameters"].get("complexity", "?") for r in records})
    sizes = sorted({i(r["metadata"]["parameters"].get("sim.block_size")) for r in records})
    predictions = sorted({r["metadata"]["parameters"].get("prediction_quality", "?") for r in records})
    correct = sum(bool(r.get("correctness", {}).get("serial_equivalent")) for r in records)
    bypassed = sum(bool(r.get("planning", {}).get("serial_bypassed", False)) for r in records)
    experiments = sorted({r["metadata"].get("experiment_id", "?") for r in records})

    lines = [
        f"records={len(records)} schema_versions={schema_versions} serial_equivalent={correct}/{len(records)}",
        f"block_sizes={','.join(map(str, sizes))}",
        f"complexities={','.join(complexities)} predictions={','.join(predictions)} bypassed_records={bypassed}",
        f"experiments={','.join(experiments)}",
        "",
        "=== Phase 3: final-DAG compression / bucketed prediction / bypass / cost feedback / VM lifecycle ===",
    ]
    lines.extend(describe(record) for record in records)
    lines += [
        "",
        "Upload this summary plus records.jsonl and aggregate/summary-wide.csv for Phase 3 analysis.",
    ]
    text = "\n".join(lines) + "\n"
    if args.output:
        Path(args.output).write_text(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
