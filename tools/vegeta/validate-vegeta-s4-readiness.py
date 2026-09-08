#!/usr/bin/env python3
"""Fail-closed publication-readiness gate for the frozen Vegeta S4 native translation.

Family-level all-source storage-access coverage and structural conflict fidelity are hard
publication gates. Conflict-relevant access and strict all-or-nothing transaction-gas
coverage remain reported diagnostics; selector-reviewed conflict, reviewed-state
transaction/conflict-participant coverage, and native implementation readiness are still
enforced after translation.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--family-coverage", type=Path, required=True)
    ap.add_argument("--translation-coverage", type=Path, required=True)
    ap.add_argument("--semantic-coverage", type=Path, required=True)
    ap.add_argument("--transaction-deficit", type=Path, required=True)
    ap.add_argument("--min-conflict", type=float, default=0.95)
    ap.add_argument("--min-median-block", type=float, default=0.80)
    ap.add_argument(
        "--min-family-storage-access", type=float, default=0.90,
        help="hard publication threshold for all source storage-access coverage",
    )
    ap.add_argument(
        "--min-family-conflict-relevant-access", type=float, default=0.90,
        help="diagnostic reference threshold for conflict-relevant source storage-access coverage",
    )
    ap.add_argument("--min-semantic-tx", type=float, default=0.80)
    ap.add_argument("--min-contention-tx", type=float, default=0.80)
    # Backward-compatible diagnostic knobs; no longer hard gates.
    ap.add_argument("--min-family-gas", type=float, default=0.90, help=argparse.SUPPRESS)
    ap.add_argument("--min-semantic-gas", type=float, default=0.90, help=argparse.SUPPRESS)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--allow-low", action="store_true")
    ns = ap.parse_args()

    fmap = load(ns.family_map)
    fam = load(ns.family_coverage)
    trans = load(ns.translation_coverage)
    sem = load(ns.semantic_coverage)
    deficit = load(ns.transaction_deficit)
    if fmap.get("candidate_only"):
        raise SystemExit("refusing candidate_only S4 family map; freeze a reviewed evaluation/vegeta/s4-native-family-map.v1.json first")

    relevant_storage = fam.get("conflict_relevant_storage_access_coverage") or {}
    storage = fam.get("storage_access_coverage") or {}
    family_gas = fam.get("gas_weighted_family_coverage") or {}
    state_denom = (deficit.get("denominators") or {}).get("source_storage_access_transactions") or {}
    metrics = {
        "family_conflict": float((fam.get("source_conflict_coverage") or {}).get("coverage", 0)),
        "family_median": float((fam.get("block_balanced_conflict_coverage") or {}).get("median_coverage") or 0),
        "family_conflict_relevant_access": float(relevant_storage.get("access_record_coverage") or 0),
        "family_storage_access": float(storage.get("access_record_coverage") or 0),
        "family_all_storage_access_diagnostic": float(storage.get("access_record_coverage") or 0),  # compatibility alias
        "family_state_owner_occurrence": float(storage.get("state_owner_occurrence_coverage") or 0),
        "family_fully_mapped_state_gas_diagnostic": float(family_gas.get("fully_selected_family_state_gas_coverage") or 0),
        "semantic_conflict": float(sem.get("coverage", 0)),
        "semantic_median": float((sem.get("block_balanced") or {}).get("median_coverage") or 0),
        "semantic_tx": float(((deficit.get("denominators") or {}).get("all_source_transactions") or {}).get("successful_reviewed_state_coverage", 0)),
        "semantic_state_gas_diagnostic": float(state_denom.get("successful_reviewed_state_gas_coverage", 0)),
        "contention_tx": float(((deficit.get("denominators") or {}).get("source_conflict_participating_transactions") or {}).get("successful_reviewed_state_coverage", 0)),
        "implementation_ready": bool((trans.get("implementation_readiness") or {}).get("native_execution_ready")),
    }
    gates = {
        "family_conflict": metrics["family_conflict"] >= ns.min_conflict,
        "family_median": metrics["family_median"] >= ns.min_median_block,
        "family_storage_access": metrics["family_storage_access"] >= ns.min_family_storage_access,
        "semantic_conflict": metrics["semantic_conflict"] >= ns.min_conflict,
        "semantic_median": metrics["semantic_median"] >= ns.min_median_block,
        "semantic_tx": metrics["semantic_tx"] >= ns.min_semantic_tx,
        "contention_tx": metrics["contention_tx"] >= ns.min_contention_tx,
        "implementation_ready": metrics["implementation_ready"],
    }
    ready = all(gates.values())
    report = {
        "schema_version": 4,
        "dataset": "vegeta-s4",
        "ready": ready,
        "metrics": metrics,
        "gates": gates,
        "thresholds": {
            "conflict": ns.min_conflict,
            "median_block": ns.min_median_block,
            "family_storage_access": ns.min_family_storage_access,
            "semantic_tx": ns.min_semantic_tx,
            "contention_tx": ns.min_contention_tx,
        },
        "diagnostic_reference_thresholds": {
            "family_conflict_relevant_access": ns.min_family_conflict_relevant_access,
            "fully_mapped_family_state_gas": ns.min_family_gas,
            "successful_reviewed_state_gas": ns.min_semantic_gas,
        },
        "diagnostic_note": (
            "All-source storage-access and family/selector conflict coverage are hard publication gates. "
            "Conflict-relevant access and all-or-nothing transaction-gas coverage remain diagnostics."
        ),
        "oracle_scope": "no exact SLOAD/SSTORE oracle; S4 is a real-trace translation/throughput workload",
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    lines = [
        "Vegeta S4 native readiness",
        "",
        f"ready: {'PASS' if ready else 'FAIL'}",
        f"family conflict coverage: {100*metrics['family_conflict']:.2f}%",
        f"family median block coverage: {100*metrics['family_median']:.2f}%",
        f"family all storage-access coverage: {100*metrics['family_storage_access']:.2f}% (target {100*ns.min_family_storage_access:.2f}%)",
        f"family conflict-relevant storage-access coverage: {100*metrics['family_conflict_relevant_access']:.2f}% (diagnostic)",
        f"family state-owner occurrence coverage: {100*metrics['family_state_owner_occurrence']:.2f}% (diagnostic)",
        f"fully mapped source-state gas: {100*metrics['family_fully_mapped_state_gas_diagnostic']:.2f}% (conservative diagnostic; not a gate)",
        f"selector-reviewed conflict coverage: {100*metrics['semantic_conflict']:.2f}%",
        f"selector-reviewed median block: {100*metrics['semantic_median']:.2f}%",
        f"successful reviewed-state tx: {100*metrics['semantic_tx']:.2f}%",
        f"successful reviewed-state source-state gas: {100*metrics['semantic_state_gas_diagnostic']:.2f}% (conservative diagnostic; not a gate)",
        f"successful reviewed-state conflict participants: {100*metrics['contention_tx']:.2f}%",
        f"implementation ready: {metrics['implementation_ready']}",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    if not ready and not ns.allow_low:
        raise SystemExit("S4 native translation is below publication readiness gates")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
