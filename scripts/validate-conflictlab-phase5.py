#!/usr/bin/env python3
"""Structural/correctness validator for the Phase 5 control-plane campaign."""

from __future__ import annotations

import argparse
import json
import statistics
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

CONTROL = "conflictlab-phase5-control-plane"
MIXED = "conflictlab-phase5-mixed-admission"
PHASE4_SYSTEM = "conflictlab-phase4-system"


def load_records(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError as error:
                raise SystemExit(f"{path}:{line_number}: invalid JSON: {error}") from error
    return records


def params(record: dict[str, Any]) -> dict[str, str]:
    return record["metadata"]["parameters"]


def experiment(record: dict[str, Any]) -> str:
    return record["metadata"]["experiment_id"]


def median(values: Iterable[float]) -> float:
    values = list(values)
    return statistics.median(values) if values else float("nan")


def pct(value: float) -> str:
    return f"{100.0 * value:.1f}%"


def comparison_key(record: dict[str, Any]) -> tuple[str, ...]:
    p = params(record)
    return (
        record["metadata"]["mode"],
        str(record["metadata"]["seed"]),
        p["sim.block_size"],
        p["complexity"],
        p.get("complexity_mix", "homogeneous"),
        p["prediction_quality"],
        p["contention"],
        p["acg.serial_bypass_enabled"],
    )


def validate(records: list[dict[str, Any]], baseline: list[dict[str, Any]] | None) -> None:
    failures: list[str] = []
    counts = Counter(experiment(record) for record in records)
    expected = {CONTROL: 432, MIXED: 48}
    if counts != expected:
        failures.append(f"experiment counts {dict(counts)} != {expected}")

    if len(records) != 480:
        failures.append(f"expected 480 records, got {len(records)}")
    if any(record.get("schema_version") != 3 for record in records):
        failures.append("every record must use schema v3")
    if any(record["correctness"].get("serial_equivalent") is not True for record in records):
        failures.append("every Phase 5 record must be serial-equivalent")
    if any(record["feedback"].get("candidate_misses", 0) != 0 for record in records):
        failures.append("candidate_misses must stay zero")
    if any(
        key.startswith("acg.exploration_")
        for record in records
        for key in params(record)
    ):
        failures.append("Phase 5 matrices must not contain exploration parameters")
    if any(params(record).get("vm_instance_lifecycle") != "reuse" for record in records):
        failures.append("Phase 5 uses reusable Wasm instances throughout")
    if any(record["execution"]["contract"].get("wasm_instance_recycles", 0) != 0 for record in records):
        failures.append("reuse-mode Phase 5 records must not recycle Wasm instances")

    bypassed = [record for record in records if record["planning"].get("serial_bypassed")]
    if not bypassed:
        failures.append("expected at least one serial bypass")
    for record in bypassed:
        planning = record["planning"]
        execution = record["execution"]
        scheduling = record["scheduling"]
        transaction_count = execution["transactions"]
        stage_fields = (
            "adapter_nanos",
            "candidate_graph_nanos",
            "scheduler_nanos",
            "schedule_validation_nanos",
            "plan_conversion_nanos",
        )
        if any(planning.get(field, 0) != 0 for field in stage_fields):
            failures.append(
                f"run {record['metadata']['run_index']}: serial bypass paid adaptive planning work"
            )
            break
        if scheduling.get("candidate_edges") != 0 or scheduling.get("materialized_candidate_edges") != 0:
            failures.append("serial bypass must not construct candidate relationships")
            break
        if execution.get("workers") != 1 or execution.get("dependency_count") != 0:
            failures.append("serial bypass must execute with one worker and zero executor dependencies")
            break
        if execution.get("hard_dependency_count") != 0:
            failures.append("serial bypass must have zero hard executor dependencies")
            break
        if execution.get("canonical_transactions") != transaction_count:
            failures.append("serial bypass must canonically execute every transaction")
            break

    b512_bucketed_adaptive = [
        record
        for record in records
        if experiment(record) == CONTROL
        and params(record)["sim.block_size"] == "512"
        and params(record)["prediction_quality"] == "bucketed"
        and params(record)["acg.serial_bypass_enabled"] == "false"
        and record["metadata"]["mode"] in {"probability-only", "cost-aware"}
    ]
    if len(b512_bucketed_adaptive) != 24:
        failures.append(
            f"expected 24 B512 bucketed adaptive/no-bypass records, got {len(b512_bucketed_adaptive)}"
        )
    compressed = [
        record
        for record in b512_bucketed_adaptive
        if record["scheduling"]["candidate_edges"]
        > record["scheduling"]["materialized_candidate_edges"]
    ]
    if len(compressed) != len(b512_bucketed_adaptive):
        failures.append("every B512 bucketed adaptive/no-bypass record must compact logical candidates")

    soft_compact = [
        record
        for record in b512_bucketed_adaptive
        if record["scheduling"].get("soft_edges", 0) > 0
    ]
    if not soft_compact:
        failures.append("expected mature Soft B512 bucketed relationships after warmup")
    elif any(
        record["scheduling"]["materialized_candidate_edges"]
        >= record["scheduling"]["candidate_edges"]
        for record in soft_compact
    ):
        failures.append("mature Soft relationships must remain compact")

    compression = [
        record["scheduling"]["candidate_edges"]
        / max(1, record["scheduling"]["materialized_candidate_edges"])
        for record in b512_bucketed_adaptive
    ]
    median_compression = median(compression)
    if compression and median_compression < 10.0:
        failures.append(
            f"B512 bucketed adaptive median materialization compression {median_compression:.1f}x < 10x"
        )

    print(
        f"Phase 5 structural checks: records={len(records)} correct={sum(r['correctness']['serial_equivalent'] is True for r in records)}/{len(records)} "
        f"bypassed={len(bypassed)} candidate_misses={sum(r['feedback']['candidate_misses'] for r in records)}"
    )
    print(
        f"B512 bucketed adaptive: {len(compressed)}/{len(b512_bucketed_adaptive)} compact, "
        f"soft_compact={len(soft_compact)}, median materialization compression={median_compression:.1f}x"
    )

    mixed = [record for record in records if experiment(record) == MIXED]
    for mix in ("80-15-5", "33-34-33", "10-30-60"):
        rows = [record for record in mixed if params(record)["complexity_mix"] == mix]
        low = [record for record in rows if params(record)["contention"] == "25pct"]
        print(
            f"admission mix={mix}: bypass={pct(sum(r['planning']['serial_bypassed'] for r in rows) / len(rows))} "
            f"low-contention bypass={pct(sum(r['planning']['serial_bypassed'] for r in low) / len(low))} "
            f"median pipeline={median(r['pipeline_timing']['end_to_end_speedup_milli'] / 1000.0 for r in rows):.2f}x"
        )

    if baseline is not None:
        old = {
            comparison_key(record): record
            for record in baseline
            if experiment(record) == PHASE4_SYSTEM
            and params(record)["sim.block_size"] == "512"
            and params(record)["prediction_quality"] == "bucketed"
            and params(record)["acg.serial_bypass_enabled"] == "false"
            and record["metadata"]["mode"] in {"probability-only", "cost-aware"}
        }
        new = {comparison_key(record): record for record in b512_bucketed_adaptive}
        shared = sorted(old.keys() & new.keys())
        if len(shared) != 24:
            failures.append(f"Phase 4 comparison expected 24 matched cells, got {len(shared)}")
        else:
            old_planning = median(old[key]["pipeline_timing"]["planning_nanos"] for key in shared)
            new_planning = median(new[key]["pipeline_timing"]["planning_nanos"] for key in shared)
            old_feedback = median(old[key]["feedback_timing"]["total_nanos"] for key in shared)
            new_feedback = median(new[key]["feedback_timing"]["total_nanos"] for key in shared)
            print(
                "Phase 4 matched B512 bucketed adaptive comparison: "
                f"planning {old_planning / 1e6:.2f}ms -> {new_planning / 1e6:.2f}ms "
                f"({old_planning / max(1.0, new_planning):.1f}x faster), "
                f"feedback {old_feedback / 1e6:.2f}ms -> {new_feedback / 1e6:.2f}ms "
                f"({old_feedback / max(1.0, new_feedback):.1f}x faster)"
            )
            if new_planning > old_planning * 0.50:
                failures.append(
                    "matched Phase 5 B512 bucketed planning did not achieve at least a 2x median reduction"
                )
            if new_feedback > old_feedback * 1.10:
                failures.append(
                    "matched Phase 5 B512 bucketed feedback regressed by more than 10% versus Phase 4"
                )

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}")
        raise SystemExit(1)
    print("PASS: Phase 5 control-plane structural/correctness validation")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--baseline", type=Path, help="optional Phase 4 records.jsonl for matched timing comparison")
    args = parser.parse_args()
    validate(load_records(args.records), load_records(args.baseline) if args.baseline else None)


if __name__ == "__main__":
    main()
