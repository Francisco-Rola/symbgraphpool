#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path

STRATEGIES = (
    "serial",
    "aria-fb",
    "vegeta",
    "exact-access",
    "static",
    "probability-only",
    "cost-aware",
)


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def read_records(path: Path) -> list[dict]:
    rows = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        row["_lineno"] = lineno
        rows.append(row)
    return rows


def validate(records: list[dict], config: dict, freeze: dict, topology: dict | None) -> dict:
    errors: list[str] = []
    expected_strategies = tuple(config["strategies"])
    if expected_strategies != STRATEGIES:
        errors.append(f"frozen config strategy order/content changed: {expected_strategies}")
    expected_blocks = list(range(int(config["block_start"]), int(config["block_end"]) + 1))
    expected_samples = int(config["samples"])
    expected_workers = int(config["workers"])
    expected_tx = int(config["transaction_count"])
    expected_count = len(expected_blocks) * len(expected_strategies) * expected_samples
    if len(records) != expected_count:
        errors.append(f"record count {len(records)} != expected {expected_count}")

    keyed: dict[tuple[int, str, int], list[dict]] = defaultdict(list)
    seen = set()
    for row in records:
        key = (int(row.get("sample", -1)), str(row.get("strategy")), int(row.get("block_number", -1)))
        if key in seen:
            errors.append(f"duplicate record {key}")
        seen.add(key)
        keyed[(key[0], key[1], int(row.get("workers", -1)))].append(row)
        if row.get("dataset") != "vegeta-s3-native-seven-strategy":
            errors.append(f"line {row.get('_lineno', '?')}: unexpected dataset {row.get('dataset')!r}")
        if row.get("evaluation_config_id") != config["experiment_id"]:
            errors.append(f"line {row.get('_lineno', '?')}: evaluation config id mismatch")
        if int(row.get("workers", -1)) != expected_workers:
            errors.append(f"line {row.get('_lineno', '?')}: workers mismatch")
        if int(row.get("consensus_cutoff_nanos", -1)) != int(config["consensus_cutoff_ms"]) * 1_000_000:
            errors.append(f"line {row.get('_lineno', '?')}: consensus cutoff mismatch")
        if abs(float(row.get("probability_threshold", -1)) - float(config["probability_threshold"])) > 1e-12:
            errors.append(f"line {row.get('_lineno', '?')}: probability threshold mismatch")
        if abs(float(row.get("cost_bypass_speedup", -1)) - float(config["cost_bypass_speedup"])) > 1e-12:
            errors.append(f"line {row.get('_lineno', '?')}: cost-bypass threshold mismatch")
        if int(row.get("strategy_order_seed", -1)) != int(config["strategy_order_seed"]):
            errors.append(f"line {row.get('_lineno', '?')}: strategy order seed mismatch")
        if not row.get("serial_equivalent"):
            errors.append(f"line {row.get('_lineno', '?')}: strategy state is not serial-equivalent")
        if str(row.get("feedback_scope")) != "strictly-prior-blocks-only":
            errors.append(f"line {row.get('_lineno', '?')}: feedback scope is not strictly prior blocks")
        if str(row.get("symbolic_source")) != "checked-in-source-derived-native-s3-profiles":
            errors.append(f"line {row.get('_lineno', '?')}: symbolic source label mismatch")
        strategy = str(row.get("strategy"))
        planning_source = str(row.get("planning_source", ""))
        if strategy == "exact-access" and not ("evaluation-only" in planning_source and "oracle" in planning_source):
            errors.append(f"line {row.get('_lineno', '?')}: exact-access is not explicitly oracle-labelled")
        if strategy == "static" and "source-derived symbolic" not in planning_source:
            errors.append(f"line {row.get('_lineno', '?')}: static strategy lacks source-derived label")
        if strategy in {"probability-only", "cost-aware"} and "strictly prior-block" not in planning_source:
            errors.append(f"line {row.get('_lineno', '?')}: adaptive strategy lacks prior-block provenance")
        if strategy == "serial" and planning_source != "none":
            errors.append(f"line {row.get('_lineno', '?')}: serial planning source should be none")
        for field in (
            "matched_serial_nanos", "strategy_total_nanos", "planning_nanos", "preexecution_nanos",
            "reconciliation_nanos", "post_consensus_nanos", "cutoff_overrun_nanos",
            "pre_consensus_nanos", "consensus_bottleneck_nanos", "feedback_nanos",
            "transactions", "prepared_receipts", "reused_receipts", "replayed_transactions",
            "canonical_transactions", "discovered_conflicts", "reference_conflicts",
            "dependency_edges", "waves", "max_wave_width",
        ):
            if int(row.get(field, -1)) < 0:
                errors.append(f"line {row.get('_lineno', '?')}: negative/missing {field}")

    for sample in range(expected_samples):
        for strategy in expected_strategies:
            group = keyed.get((sample, strategy, expected_workers), [])
            blocks = sorted(int(r["block_number"]) for r in group)
            if blocks != expected_blocks:
                errors.append(f"sample={sample} strategy={strategy}: block coverage mismatch")
            txs = sum(int(r["transactions"]) for r in group)
            if txs != expected_tx:
                errors.append(f"sample={sample} strategy={strategy}: transactions {txs} != {expected_tx}")

    if not freeze.get("freeze_ready"):
        errors.append("candidate-archetype freeze is not ready")
    if topology is not None:
        if not topology.get("accepted"):
            errors.append("exact family-extension/topology prerequisite validation is not accepted")
        if not topology.get("frozen_gate_status"):
            errors.append("exact family-extension frozen gates are not passing")

    return {
        "schema_version": 1,
        "dataset": "vegeta-s3-native-scheduler-validation",
        "accepted": not errors,
        "errors": errors,
        "mechanical_checks": {
            "records": len(records),
            "expected_records": expected_count,
            "samples": expected_samples,
            "blocks_per_strategy_sample": len(expected_blocks),
            "transactions_per_strategy_sample": expected_tx,
            "workers": expected_workers,
            "candidate_archetype_freeze_ready": bool(freeze.get("freeze_ready")),
            "topology_prerequisite_accepted": None if topology is None else bool(topology.get("accepted")),
        },
        "performance_threshold_policy": "none: speedup, latency, replay, reuse, and bypass are report-only outcomes",
    }


def render(report: dict) -> str:
    m = report["mechanical_checks"]
    lines = [
        "Vegeta S3 native scheduler result validation",
        "",
        f"accepted: {'yes' if report['accepted'] else 'NO'}",
        f"records: {m['records']} / {m['expected_records']}",
        f"samples: {m['samples']}",
        f"blocks per strategy/sample: {m['blocks_per_strategy_sample']}",
        f"transactions per strategy/sample: {m['transactions_per_strategy_sample']}",
        f"workers: {m['workers']}",
        f"candidate-archetype freeze ready: {'yes' if m['candidate_archetype_freeze_ready'] else 'NO'}",
        f"topology prerequisite accepted: {m['topology_prerequisite_accepted']}",
        "",
        report["performance_threshold_policy"],
    ]
    if report["errors"]:
        lines.extend(["", "errors:"] + [f"  - {error}" for error in report["errors"]])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--records", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--freeze", type=Path, required=True)
    parser.add_argument("--topology-validation", type=Path, default=None)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    records = read_records(args.records)
    report = validate(
        records,
        read_json(args.config),
        read_json(args.freeze),
        read_json(args.topology_validation) if args.topology_validation else None,
    )
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "validation.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    text = render(report)
    (args.output_dir / "validation.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    return 0 if report["accepted"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
