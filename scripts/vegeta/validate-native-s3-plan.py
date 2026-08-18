#!/usr/bin/env python3
"""Validate Vegeta S3 native pre-execution translation artifacts.

Validation checks exact block/transaction retention, order/hash identity, absence of concrete
historical read/write sets from the native plan, and the frozen pre-execution fidelity policy. The
aggregate/block-balanced conflict metrics come from ``translation-coverage.json``. Semantic
transaction/frame gates come from ``final-mapping-simulation.json`` once the selector-granular
background finalization pass has run. ``--require-execution-ready`` additionally requires all seven
base native contract sources and genuine LLM symbolic analyses to exist.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from vegeta_corpus import load_blocks

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CORPUS = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"
DEFAULT_PLAN_DIR = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan"
DEFAULT_MAP = ROOT / "evaluation/vegeta/s3-native-family-map.v1.json"
DEFAULT_GATE = ROOT / "evaluation/vegeta/s3-native-preexecution-gates.v1.json"


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def forbidden_access_path(value: Any, path: str = "$PLAN") -> str | None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key in {"reads", "writes", "actual_reads", "actual_writes", "concrete_accesses"}:
                return f"{path}.{key}"
            found = forbidden_access_path(child, f"{path}.{key}")
            if found:
                return found
    elif isinstance(value, list):
        for index, child in enumerate(value):
            found = forbidden_access_path(child, f"{path}[{index}]")
            if found:
                return found
    return None


def gate_measurements(coverage: dict, simulation: dict | None = None) -> dict[str, float | None]:
    conflict = coverage.get("source_conflict_coverage") or {}
    balanced = coverage.get("block_balanced_conflict_coverage") or {}
    tx_semantic = coverage.get("transaction_semantic_coverage") or {}
    calls = coverage.get("calls") or {}
    simulated = (simulation or {}).get("simulation") or simulation or {}
    return {
        "aggregate_source_conflict_coverage": conflict.get("coverage"),
        "median_conflict_bearing_block_coverage": balanced.get("median_coverage"),
        "semantic_transaction_coverage": simulated.get(
            "semantic_transaction_coverage", tx_semantic.get("semantic_transaction_coverage")
        ),
        "semantic_call_frame_coverage": simulated.get(
            "semantic_call_frame_coverage", calls.get("semantic_frame_coverage")
        ),
    }


def evaluate_gate_config(
    coverage: dict, gate_config: dict | None, simulation: dict | None = None
) -> list[dict]:
    values = gate_measurements(coverage, simulation)
    metrics = (gate_config or {}).get("metrics") or {}
    rows = []
    for name, value in values.items():
        rule = metrics.get(name) or {}
        minimum = rule.get("minimum")
        enforced = bool(rule.get("enforced"))
        if minimum is None:
            status = "threshold-unset"
            passed = None
        elif value is None:
            status = "measurement-missing"
            passed = False
        else:
            passed = float(value) + 1e-12 >= float(minimum)
            status = "pass" if passed else "fail"
        rows.append({
            "metric": name,
            "measured": value,
            "minimum": minimum,
            "enforced": enforced,
            "passed": passed,
            "status": status,
            "reason": rule.get("reason"),
            "measurement_source": (
                "final-mapping-simulation"
                if name in {"semantic_transaction_coverage", "semantic_call_frame_coverage"} and simulation is not None
                else "translation-coverage"
            ),
        })
    return rows


def render_gate_report(gates: list[dict], policy_status: str | None) -> str:
    lines = [
        "Vegeta S3 native pre-execution gate report",
        "",
        f"policy status: {policy_status or '-'}",
    ]
    for gate in gates:
        measured = gate["measured"]
        minimum = gate["minimum"]
        measured_text = "n/a" if measured is None else f"{float(measured) * 100:.2f}%"
        minimum_text = "unset" if minimum is None else f"{float(minimum) * 100:.2f}%"
        lines.append(
            f"{gate['metric']}: measured={measured_text} minimum={minimum_text} "
            f"enforced={'yes' if gate['enforced'] else 'no'} status={gate['status']}"
        )
    lines.extend([
        "",
        "All four pre-execution workload-fidelity thresholds are frozen. Semantic-volume gates use the selector-granular final mapping simulation when present.",
    ])
    return "\n".join(lines) + "\n"


def validate_plan(
    source_blocks: list[dict],
    plan_blocks: list[dict],
    coverage: dict,
    frozen_map: dict,
    require_execution_ready: bool = False,
    gate_config: dict | None = None,
    simulation: dict | None = None,
) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []

    if len(source_blocks) != len(plan_blocks):
        errors.append(f"block retention mismatch: source={len(source_blocks)} plan={len(plan_blocks)}")

    for position, (source, plan) in enumerate(zip(source_blocks, plan_blocks)):
        source_number = int(source.get("block_number", -1))
        plan_number = int(plan.get("block_number", -2))
        if source_number != plan_number:
            errors.append(f"block position {position}: source block {source_number} != plan block {plan_number}")
            continue
        if str(source.get("block_hash") or "").lower() != str(plan.get("block_hash") or "").lower():
            errors.append(f"block {source_number}: block hash mismatch")
        source_txs = source.get("transactions") or []
        plan_txs = plan.get("transactions") or []
        if len(source_txs) != len(plan_txs):
            errors.append(f"block {source_number}: source tx={len(source_txs)} plan tx={len(plan_txs)}")
            continue
        for index, (source_tx, plan_tx) in enumerate(zip(source_txs, plan_txs)):
            if int(plan_tx.get("tx_index", -1)) != index or int(source_tx.get("tx_index", -2)) != index:
                errors.append(f"block {source_number} tx {index}: tx index/order mismatch")
            if str(source_tx.get("tx_hash") or "").lower() != str(plan_tx.get("tx_hash") or "").lower():
                errors.append(f"block {source_number} tx {index}: transaction hash mismatch")
            if plan_tx.get("translation_class") not in {
                "fully-semantic", "mixed-semantic-fallback", "background-only"
            }:
                errors.append(f"block {source_number} tx {index}: invalid translation_class")
            actions = plan_tx.get("native_actions")
            if not isinstance(actions, list) or not actions:
                errors.append(f"block {source_number} tx {index}: native_actions must retain at least root frame")
                continue
            for action_index, action in enumerate(actions):
                if action.get("action_id") != action_index:
                    errors.append(f"block {source_number} tx {index}: action ids are not contiguous")
                if action.get("translation_status") not in {
                    "mapped-native-call", "mapped-system-action", "inlined-delegatecall", "background-fallback"
                }:
                    errors.append(
                        f"block {source_number} tx {index} action {action_index}: invalid translation_status"
                    )

    forbidden = forbidden_access_path(plan_blocks)
    if forbidden:
        errors.append(f"prediction leakage guard failed: concrete access field found at {forbidden}")

    source_transactions = sum(len(block.get("transactions") or []) for block in source_blocks)
    plan_transactions = sum(len(block.get("transactions") or []) for block in plan_blocks)
    if source_transactions != plan_transactions:
        errors.append(f"transaction retention mismatch: source={source_transactions} plan={plan_transactions}")
    if abs(float(coverage.get("transaction_retention", 0.0)) - 1.0) > 1e-12:
        errors.append(f"coverage report transaction_retention={coverage.get('transaction_retention')} != 1.0")

    profile_mappings = frozen_map.get("profile_mappings") or []
    native_families = frozen_map.get("native_code_families") or {}
    if len(profile_mappings) != 11:
        errors.append(f"frozen family map has {len(profile_mappings)} profile mappings; expected 11")
    if len(native_families) != 7:
        errors.append(f"frozen family map has {len(native_families)} native code families; expected 7")

    conflict = coverage.get("source_conflict_coverage") or {}
    measured = float(conflict.get("coverage", 0.0))
    target = float(frozen_map.get("target_conflict_coverage", 0.95))
    if measured + 1e-12 < target:
        errors.append(f"source selected-family conflict coverage {measured:.6f} < target {target:.6f}")

    if gate_config is not None:
        gates = evaluate_gate_config(coverage, gate_config, simulation)
        for gate in gates:
            name = gate["metric"]
            if gate["enforced"] and gate["minimum"] is None:
                errors.append(f"gate {name} is enforced but has no minimum")
            elif gate["enforced"] and gate["passed"] is not True:
                errors.append(
                    f"pre-execution gate {name}={gate['measured']} < minimum {gate['minimum']}"
                )

    topology = coverage.get("native_topology_fidelity") or {}
    if topology.get("status") != "not-measured-preexecution":
        errors.append("native topology fidelity must remain unmeasured in the pre-execution planner")

    readiness = coverage.get("implementation_readiness") or {}
    execution_ready = bool(readiness.get("native_execution_ready"))
    if require_execution_ready and not execution_ready:
        errors.append(
            "execution-ready gate failed: missing native contract source(s) and/or genuine symbolic analyses"
        )
    elif not execution_ready:
        warnings.append(
            "translation integrity passes, but native execution is intentionally gated until all seven contract sources and genuine LLM symbolic analyses exist"
        )

    background = int((coverage.get("calls") or {}).get("background_fallback_frames", 0))
    if background:
        warnings.append(
            f"{background} call frames remain explicit background fallbacks; they were retained rather than deleted"
        )
    return errors, warnings


def render_text(report: dict) -> str:
    lines = [
        "Vegeta S3 native translation validation",
        "",
        f"accepted: {'yes' if report['accepted'] else 'no'}",
        f"execution-ready required: {'yes' if report['require_execution_ready'] else 'no'}",
        f"errors: {len(report['errors'])}",
        f"warnings: {len(report['warnings'])}",
    ]
    for error in report["errors"]:
        lines.append(f"ERROR: {error}")
    for warning in report["warnings"]:
        lines.append(f"WARN: {warning}")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--plan-dir", type=Path, default=DEFAULT_PLAN_DIR)
    parser.add_argument("--family-map", type=Path, default=DEFAULT_MAP)
    parser.add_argument("--gate-config", type=Path, default=DEFAULT_GATE)
    parser.add_argument("--simulation", type=Path, default=None)
    parser.add_argument("--require-execution-ready", action="store_true")
    args = parser.parse_args()

    source = load_blocks(args.corpus)
    plan = load_blocks(args.plan_dir / "native-plan.jsonl")
    coverage = read_json(args.plan_dir / "translation-coverage.json")
    frozen = read_json(args.family_map)
    gate_config = read_json(args.gate_config) if args.gate_config.exists() else None
    simulation_path = args.simulation or (args.plan_dir / "final-mapping-simulation.json")
    simulation = read_json(simulation_path) if simulation_path.exists() else None
    errors, warnings = validate_plan(
        source, plan, coverage, frozen, args.require_execution_ready,
        gate_config=gate_config, simulation=simulation,
    )
    gates = evaluate_gate_config(coverage, gate_config, simulation)
    report = {
        "schema_version": 2,
        "accepted": not errors,
        "require_execution_ready": args.require_execution_ready,
        "gate_config": str(args.gate_config),
        "gate_policy_status": (gate_config or {}).get("policy_status"),
        "simulation": str(simulation_path) if simulation is not None else None,
        "preexecution_gates": gates,
        "errors": errors,
        "warnings": warnings,
    }
    write_json_atomic(args.plan_dir / "validation-report.json", report)
    write_json_atomic(args.plan_dir / "preexecution-gate-report.json", {
        "schema_version": 1,
        "dataset": coverage.get("dataset", "vegeta-s3"),
        "policy_status": (gate_config or {}).get("policy_status"),
        "gates": gates,
    })
    (args.plan_dir / "preexecution-gate-report.txt").write_text(
        render_gate_report(gates, (gate_config or {}).get("policy_status")), encoding="utf-8"
    )
    (args.plan_dir / "validation-report.txt").write_text(render_text(report), encoding="utf-8")
    print(render_text(report), end="")
    return 0 if not errors else 2


if __name__ == "__main__":
    raise SystemExit(main())
