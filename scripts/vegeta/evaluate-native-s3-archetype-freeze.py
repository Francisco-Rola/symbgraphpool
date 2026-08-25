#!/usr/bin/env python3
"""Freeze check for Vegeta S3's two historically-labelled candidate archetypes.

This evaluator deliberately answers two separate questions:

1. Would the previously frozen semantic-volume gates still pass if the two archetypes were
   excluded from selector-granular reuse?
2. If not, are those archetypes now backed by real checked-in CosmWasm implementations and genuine
   source-derived symbolic profiles rather than remaining simulation-only placeholders?

The result is diagnostic/freeze metadata only. It does not alter any frozen threshold and does not
consume historical concrete Ethereum read/write keys.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
FINALIZER = ROOT / "scripts/vegeta/finalize-native-s3-map.py"
DEFAULT_PLAN_DIR = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan"
DEFAULT_CHAR = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/characterization"
DEFAULT_GATE = ROOT / "evaluation/vegeta/s3-native-preexecution-gates.v1.json"
DEFAULT_IMPL = ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json"
CANDIDATES = ("cw1155-like", "operator-filter-helper")


def load_finalizer():
    spec = importlib.util.spec_from_file_location("vegeta_finalize_native_s3_map", FINALIZER)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {FINALIZER}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def implementation_evidence(manifest: dict, family: str) -> dict:
    rows = {
        row.get("native_code_family"): row
        for row in manifest.get("families", [])
        if isinstance(row, dict)
    }
    row = rows.get(family) or {}
    source = ROOT / str(row.get("source", ""))
    symbolic = ROOT / str(row.get("symbolic_analysis", ""))
    symbolic_doc = read_json(symbolic) if symbolic.is_file() else {}
    provenance = symbolic_doc.get("analysis_provenance") or {}
    passed = (
        row.get("implementation_status") == "implemented"
        and row.get("symbolic_status") == "llm-source-derived"
        and source.is_file()
        and symbolic.is_file()
        and provenance.get("method") == "llm-source-derived"
        and provenance.get("historical_trace_keys_used") is False
    )
    return {
        "family": family,
        "implemented": row.get("implementation_status") == "implemented",
        "source_derived_symbolic": row.get("symbolic_status") == "llm-source-derived",
        "source_exists": source.is_file(),
        "symbolic_exists": symbolic.is_file(),
        "symbolic_method": provenance.get("method"),
        "historical_trace_keys_used": provenance.get("historical_trace_keys_used"),
        "passed": passed,
    }


def gate_summary(rows: list[dict]) -> dict:
    by_name = {row["metric"]: row for row in rows}
    semantic = [
        by_name["semantic_transaction_coverage"],
        by_name["semantic_call_frame_coverage"],
    ]
    return {
        "semantic_gates_pass": all((not row.get("enforced")) or row.get("passed") for row in semantic),
        "all_frozen_gates_pass": all((not row.get("enforced")) or row.get("passed") for row in rows),
        "gates": rows,
    }


def freeze_decision(excluded_both_semantic_gates_pass: bool, implementations_pass: bool) -> tuple[bool, str]:
    if excluded_both_semantic_gates_pass:
        return True, "candidate-independent: both semantic-volume gates still pass with both archetypes excluded"
    if implementations_pass:
        return True, "candidate-dependent but implementation-closed: both archetypes are real CosmWasm + source-derived symbolic families"
    return False, "not freeze-ready: candidate-dependent coverage remains backed by an unimplemented or non-source-derived archetype"


def evaluate(
    plan_dir: Path,
    characterization_dir: Path,
    gate_config: Path,
    implementation_manifest: Path,
) -> dict:
    f = load_finalizer()
    plan = f.load_blocks(plan_dir / "native-plan.jsonl")
    code_cache = f.load_code_cache(characterization_dir / "code-cache.json")
    selector_doc = read_json(plan_dir / "selector-semantic-map.json")
    rules = selector_doc.get("rules") or []
    coverage = read_json(plan_dir / "translation-coverage.json")
    gates = read_json(gate_config)
    impl = read_json(implementation_manifest)

    cases = []
    exclusions = [(), (CANDIDATES[0],), (CANDIDATES[1],), CANDIDATES]
    for excluded in exclusions:
        filtered = [row for row in rules if row.get("native_code_family") not in set(excluded)]
        simulation = f.simulate(plan, code_cache, filtered)
        rows = f.gate_rows(coverage, simulation, gates)
        cases.append({
            "excluded_archetypes": list(excluded),
            "selector_rules": len(filtered),
            "semantic_transactions": simulation["semantic_transactions"],
            "transactions": simulation["transactions"],
            "semantic_transaction_coverage": simulation["semantic_transaction_coverage"],
            "semantic_frames": simulation["semantic_frames"],
            "total_frames": simulation["total_frames"],
            "semantic_call_frame_coverage": simulation["semantic_call_frame_coverage"],
            **gate_summary(rows),
        })

    both = next(row for row in cases if tuple(row["excluded_archetypes"]) == CANDIDATES)
    evidence = [implementation_evidence(impl, family) for family in CANDIDATES]
    implementations_pass = all(row["passed"] for row in evidence)
    freeze_ready, rationale = freeze_decision(
        bool(both["semantic_gates_pass"]), implementations_pass
    )
    return {
        "schema_version": 1,
        "dataset": "vegeta-s3-native-archetype-freeze",
        "candidate_archetypes": list(CANDIDATES),
        "cases": cases,
        "implementation_evidence": evidence,
        "excluded_both_semantic_gates_pass": both["semantic_gates_pass"],
        "candidate_implementations_pass": implementations_pass,
        "freeze_ready": freeze_ready,
        "rationale": rationale,
        "threshold_policy": "uses only the previously frozen pre-execution gates; no new threshold is introduced",
        "leakage_policy": "selector/runtime/source metadata and checked-in native source only; no historical concrete EVM read/write keys",
    }


def render(report: dict) -> str:
    lines = [
        "Vegeta S3 candidate-archetype exclusion / freeze check",
        "",
        "Previously-labelled candidate archetypes: " + ", ".join(report["candidate_archetypes"]),
        "",
        "Semantic-volume exclusion ablation:",
    ]
    for case in report["cases"]:
        label = ",".join(case["excluded_archetypes"]) or "none"
        lines.append(
            f"  exclude={label:<36} tx={case['semantic_transaction_coverage']*100:6.2f}% "
            f"frames={case['semantic_call_frame_coverage']*100:6.2f}% "
            f"semantic-gates={'PASS' if case['semantic_gates_pass'] else 'FAIL'}"
        )
    lines.extend(["", "Implementation closure:"])
    for row in report["implementation_evidence"]:
        lines.append(
            f"  {row['family']:<24} implemented={row['implemented']} "
            f"source-derived-symbolic={row['source_derived_symbolic']} "
            f"trace-keys-used={row['historical_trace_keys_used']} "
            f"status={'PASS' if row['passed'] else 'FAIL'}"
        )
    lines.extend([
        "",
        f"freeze ready: {'yes' if report['freeze_ready'] else 'no'}",
        f"rationale: {report['rationale']}",
        "",
        report["threshold_policy"],
        report["leakage_policy"],
    ])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-dir", type=Path, default=DEFAULT_PLAN_DIR)
    parser.add_argument("--characterization-dir", type=Path, default=DEFAULT_CHAR)
    parser.add_argument("--gate-config", type=Path, default=DEFAULT_GATE)
    parser.add_argument("--implementation-manifest", type=Path, default=DEFAULT_IMPL)
    parser.add_argument("--output-dir", type=Path, default=None)
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()
    out = args.output_dir or args.plan_dir / "archetype-freeze"
    report = evaluate(args.plan_dir, args.characterization_dir, args.gate_config, args.implementation_manifest)
    out.mkdir(parents=True, exist_ok=True)
    (out / "candidate-archetype-freeze.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    text = render(report)
    (out / "candidate-archetype-freeze.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    if args.strict and not report["freeze_ready"]:
        print("FAIL: Vegeta S3 candidate-archetype freeze check did not close", flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
