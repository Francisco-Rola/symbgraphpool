#!/usr/bin/env python3
"""Human-readable Phase 4 ConflictLab summary.

Phase 4 focuses on true serial fallback, reusable Wasm instances, compact candidate construction,
complexity-aware admission, targeted exploration, and heterogeneous transaction complexity.
"""

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


def short_experiment(record: dict) -> str:
    return record["metadata"].get("experiment_id", "?").replace("conflictlab-phase4-", "")


def record_key(record: dict) -> tuple:
    p = record["metadata"]["parameters"]
    return (
        short_experiment(record),
        i(p.get("sim.block_size")),
        p.get("complexity_mix", "homogeneous"),
        p.get("complexity", "?"),
        p.get("prediction_quality", "?"),
        p.get("contention", "?"),
        p.get("vm_instance_lifecycle", "reuse"),
        p.get("acg.serial_bypass_enabled", "?"),
        f(p.get("acg.risk_budget")),
        f(p.get("acg.exploration_rate")),
        f(p.get("acg.exploration_min_uncertainty")),
        i(p.get("acg.exploration_max_transactions_per_block")),
        record["metadata"].get("mode", "?"),
        i(record["metadata"].get("seed")),
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
    logical = i(sched.get("candidate_edges"))
    materialized = i(sched.get("materialized_candidate_edges", logical))
    final = i(sched.get("scheduled_dependencies"))
    materialization_compression = logical / max(materialized, 1) if logical else 0.0
    final_compression = logical / max(final, 1) if logical else 0.0
    acquire_ns = i(contract.get("aggregate_wasm_instance_acquire_nanos"))
    recycle_ns = i(contract.get("aggregate_wasm_recycle_nanos"))
    lifecycle_ns = acquire_ns + recycle_ns
    request_ns = max(i(contract.get("aggregate_request_execution_nanos")), 1)
    lifecycle_share = lifecycle_ns / request_ns
    pipeline_speedup = i(pipeline.get("end_to_end_speedup_milli")) / 1000.0
    serial_ms = i(pipeline.get("serial_reference_execution_nanos")) / 1e6
    total_ms = i(pipeline.get("total_adaptive_block_nanos")) / 1e6
    feedback_ms = i(feedback_timing.get("total_nanos")) / 1e6
    bypassed = bool(planning.get("serial_bypassed", False))

    projection = planning.get("serial_bypass_projected_speedup_milli")
    projection_text = "-" if projection is None else f"{i(projection) / 1000.0:.2f}x"
    admission = planning.get("serial_bypass_admission_score_milli")
    admission_text = "-" if admission is None else f"{i(admission) / 1000.0:.2f}x"
    service = planning.get("serial_bypass_mean_service_nanos")
    service_text = "-" if service is None else f"{i(service) / 1e3:.0f}us"

    mix = p.get("complexity_mix", "homogeneous")
    complexity = p.get("complexity", "?")
    lifecycle = p.get("vm_instance_lifecycle", "reuse")
    seed = i(record["metadata"].get("seed"))
    return (
        f"exp={short_experiment(record):<12} seed={seed:<3} B{i(p.get('sim.block_size')):<3} "
        f"cx={complexity:<6} mix={mix:<8} pred={p.get('prediction_quality','?'):<8} "
        f"cont={p.get('contention','?'):<5} vm={lifecycle:<7} "
        f"bypass_cfg={p.get('acg.serial_bypass_enabled','?'):<5} risk={f(p.get('acg.risk_budget')):.2f} "
        f"explore={f(p.get('acg.exploration_rate')):.2f}/u{f(p.get('acg.exploration_min_uncertainty')):.2f}/"
        f"n{i(p.get('acg.exploration_max_transactions_per_block')):<2} "
        f"{record['metadata'].get('mode','?'):<16} bypassed={str(bypassed):<5} "
        f"proj/adm/svc={projection_text}/{admission_text}/{service_text} "
        f"cand={logical}/{materialized}/{final} matC={materialization_compression:.1f}x finalC={final_compression:.1f}x "
        f"waves={i(sched.get('wave_count'))} replay={i(execution.get('replayed_transactions'))} "
        f"serObs/batch={i(feedback.get('serialization_cost_observations'))}/"
        f"{i(feedback.get('serialization_cost_batches_applied'))} feedback={feedback_ms:.3f}ms "
        f"vmHit/miss/recycle={i(contract.get('wasm_instance_reuse_hits'))}/"
        f"{i(contract.get('wasm_instance_pool_misses'))}/{i(contract.get('wasm_instance_recycles'))} "
        f"vmLife={lifecycle_ns / 1e6:.3f}ms ({lifecycle_ns / n / 1e3:.1f}us/tx,{lifecycle_share*100:.1f}%) "
        f"pipeline={pipeline_speedup:.2f}x total={total_ms:.3f}ms serial={serial_ms:.3f}ms"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records")
    parser.add_argument("--output")
    args = parser.parse_args()

    records = [
        json.loads(line)
        for line in Path(args.records).read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    if not records:
        raise SystemExit("no records")
    records.sort(key=record_key)

    schema_versions = sorted({i(r.get("schema_version")) for r in records})
    sizes = sorted({i(r["metadata"]["parameters"].get("sim.block_size")) for r in records})
    complexities = sorted({r["metadata"]["parameters"].get("complexity", "?") for r in records})
    mixes = sorted({r["metadata"]["parameters"].get("complexity_mix", "homogeneous") for r in records})
    predictions = sorted({r["metadata"]["parameters"].get("prediction_quality", "?") for r in records})
    lifecycles = sorted({r["metadata"]["parameters"].get("vm_instance_lifecycle", "reuse") for r in records})
    correct = sum(bool(r.get("correctness", {}).get("serial_equivalent")) for r in records)
    bypassed = sum(bool(r.get("planning", {}).get("serial_bypassed", False)) for r in records)
    experiments = sorted({r["metadata"].get("experiment_id", "?") for r in records})

    lines = [
        f"records={len(records)} schema_versions={schema_versions} serial_equivalent={correct}/{len(records)}",
        f"block_sizes={','.join(map(str, sizes))}",
        f"complexities={','.join(complexities)} complexity_mixes={','.join(mixes)}",
        f"predictions={','.join(predictions)} vm_lifecycles={','.join(lifecycles)} bypassed_records={bypassed}",
        f"experiments={','.join(experiments)}",
        "",
        "=== Phase 4: true serial fallback / VM reuse / compact planning / targeted exploration / complexity-aware admission ===",
    ]
    lines.extend(describe(record) for record in records)
    lines += [
        "",
        "Upload this summary plus records.jsonl and aggregate/summary-wide.csv for Phase 4 analysis.",
    ]
    text = "\n".join(lines) + "\n"
    if args.output:
        Path(args.output).write_text(text, encoding="utf-8")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
