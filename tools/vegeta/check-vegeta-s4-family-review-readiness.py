#!/usr/bin/env python3
"""Check whether an S4 reviewed family-map draft is ready to freeze.

This is the S4 analogue of the S1 scheduler-fidelity family gate.  Family freeze is
about dependency/contention structure, not general replay completeness: corpus integrity,
exact source conflict coverage, and block-balanced conflict coverage are hard gates.
All-source storage-access coverage, conflict-relevant access, state-owner occurrence, and
strict fully-mapped transaction/gas statistics remain visible diagnostics.  Selector,
transaction, contention, and implementation readiness are enforced after freeze.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def read(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--coverage", type=Path, required=True)
    ap.add_argument("--provenance", type=Path, required=True)
    ap.add_argument("--min-conflict", type=float, default=0.95)
    ap.add_argument("--min-median-block", type=float, default=0.80)
    ap.add_argument(
        "--storage-access-reference", "--min-storage-access", dest="storage_access_reference", type=float, default=0.90,
        help="diagnostic reference for all source storage-access coverage (not a family-freeze gate)",
    )
    ap.add_argument(
        "--min-conflict-relevant-access", type=float, default=0.90,
        help="diagnostic reference threshold for conflict-relevant access coverage",
    )
    ap.add_argument("--min-state-gas", type=float, dest="legacy_min_state_gas", help=argparse.SUPPRESS)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--allow-low", action="store_true")
    ns = ap.parse_args()

    fmap = read(ns.family_map)
    coverage = read(ns.coverage)
    provenance = read(ns.provenance)
    relevant_storage = coverage.get("conflict_relevant_storage_access_coverage") or {}
    storage = coverage.get("storage_access_coverage") or {}
    gas = coverage.get("gas_weighted_family_coverage") or {}
    metrics = {
        "internal_corpus_integrity": bool((provenance.get("internal_integrity") or {}).get("pass")),
        "conflict_coverage": float((coverage.get("source_conflict_coverage") or {}).get("coverage", 0.0)),
        "median_block_conflict_coverage": float((coverage.get("block_balanced_conflict_coverage") or {}).get("median_coverage") or 0.0),
        "conflict_relevant_access_coverage": float(relevant_storage.get("access_record_coverage") or 0.0),
        "storage_access_coverage": float(storage.get("access_record_coverage") or 0.0),
        "state_owner_occurrence_coverage": float(storage.get("state_owner_occurrence_coverage") or 0.0),
        "fully_mapped_source_state_gas_coverage_diagnostic": float(gas.get("fully_selected_family_state_gas_coverage") or 0.0),
        "fully_mapped_source_state_transaction_coverage_diagnostic": float(gas.get("fully_selected_family_state_transaction_coverage") or 0.0),
        "profile_mappings": len(fmap.get("profile_mappings") or []),
    }
    gates = {
        "internal_corpus_integrity": metrics["internal_corpus_integrity"],
        "conflict_coverage": metrics["conflict_coverage"] >= ns.min_conflict,
        "median_block_conflict_coverage": metrics["median_block_conflict_coverage"] >= ns.min_median_block,
    }
    ready = all(gates.values())
    report = {
        "schema_version": 5,
        "dataset": "vegeta-s4",
        "selected_profile": "scheduler-fidelity-family-freeze",
        "ready_to_freeze_family_map": ready,
        "metrics": metrics,
        "gates": gates,
        "thresholds": {
            "conflict": ns.min_conflict,
            "median_block": ns.min_median_block,
        },
        "diagnostic_reference_thresholds": {
            "storage_access": ns.storage_access_reference,
            "conflict_relevant_access": ns.min_conflict_relevant_access,
        },
        "definition": (
            "S4 family freeze mirrors S1 scheduler-fidelity: dependency/contention structure is gated by corpus integrity, "
            "reviewed source conflict coverage, and block-balanced conflict coverage. All-source storage volume is not a "
            "scheduler-fidelity gate and remains a transparent diagnostic."
        ),
        "non_substitution": (
            "Passing the family-freeze scheduler-fidelity gate does not imply general Ethereum semantic equivalence or "
            "all-transaction replay coverage; selector/transaction/contention/implementation readiness is enforced after freeze."
        ),
        "next_gate": "after freeze, prepare-native applies the S1-analogous scheduler-fidelity and semantic-replay readiness profiles",
    }
    atomic(ns.output, report)
    lines = [
        "Vegeta S4 family-review scheduler-fidelity freeze gate",
        "",
        f"ready to freeze: {'PASS' if ready else 'FAIL'}",
        f"internal frozen-corpus integrity: {metrics['internal_corpus_integrity']}",
        f"conflict coverage: {100*metrics['conflict_coverage']:.2f}% (target {100*ns.min_conflict:.2f}%)",
        f"median conflict-bearing block coverage: {100*metrics['median_block_conflict_coverage']:.2f}% (target {100*ns.min_median_block:.2f}%)",
        "",
        "Diagnostics (reported, not family-freeze gates):",
        f"  all storage-access coverage: {100*metrics['storage_access_coverage']:.2f}% (reference {100*ns.storage_access_reference:.2f}%)",
        f"  conflict-relevant storage-access coverage: {100*metrics['conflict_relevant_access_coverage']:.2f}% (reference {100*ns.min_conflict_relevant_access:.2f}%)",
        f"  state-owner occurrence coverage: {100*metrics['state_owner_occurrence_coverage']:.2f}%",
        f"  fully mapped source-state gas: {100*metrics['fully_mapped_source_state_gas_coverage_diagnostic']:.2f}% (conservative)",
        f"  fully mapped source-state transactions: {100*metrics['fully_mapped_source_state_transaction_coverage_diagnostic']:.2f}% (conservative)",
        f"  profile mappings: {metrics['profile_mappings']}",
        "",
        "Interpretation: this is the family-level prerequisite for the S4 contention/scheduler benchmark.",
        "It does not claim general replay completeness; prepare-native still enforces reviewed selector semantics,",
        "conflict-participant transaction coverage, and native implementation readiness.",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    if not ready and not ns.allow_low:
        raise SystemExit("S4 reviewed family map is below scheduler-fidelity family-freeze gates; continue conflict-focused review")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
