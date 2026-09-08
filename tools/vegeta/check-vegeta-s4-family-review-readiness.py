#!/usr/bin/env python3
"""Check whether an S4 reviewed family-map draft is ready to freeze.

This gate is intentionally earlier than full native-plan readiness. It requires the
frozen corpus to be internally consistent, all-source storage-access coverage to reach
the publication threshold, and structural conflict coverage to be high. Conflict-relevant
access and strict fully-mapped transaction-gas statistics remain visible diagnostics.
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
        "--min-storage-access", type=float, default=0.90,
        help="hard family-freeze threshold for coverage of all concrete source storage-access records",
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
        "all_storage_access_coverage_diagnostic": float(storage.get("access_record_coverage") or 0.0),  # compatibility alias
        "state_owner_occurrence_coverage": float(storage.get("state_owner_occurrence_coverage") or 0.0),
        "fully_mapped_source_state_gas_coverage_diagnostic": float(gas.get("fully_selected_family_state_gas_coverage") or 0.0),
        "fully_mapped_source_state_transaction_coverage_diagnostic": float(gas.get("fully_selected_family_state_transaction_coverage") or 0.0),
        "profile_mappings": len(fmap.get("profile_mappings") or []),
    }
    gates = {
        "internal_corpus_integrity": metrics["internal_corpus_integrity"],
        "storage_access_coverage": metrics["storage_access_coverage"] >= ns.min_storage_access,
        "conflict_coverage": metrics["conflict_coverage"] >= ns.min_conflict,
        "median_block_conflict_coverage": metrics["median_block_conflict_coverage"] >= ns.min_median_block,
    }
    ready = all(gates.values())
    report = {
        "schema_version": 4,
        "dataset": "vegeta-s4",
        "ready_to_freeze_family_map": ready,
        "metrics": metrics,
        "gates": gates,
        "thresholds": {
            "conflict": ns.min_conflict,
            "median_block": ns.min_median_block,
            "storage_access": ns.min_storage_access,
        },
        "diagnostic_reference_thresholds": {
            "conflict_relevant_access": ns.min_conflict_relevant_access,
        },
        "diagnostic_note": (
            "All-source storage-access and exact source-conflict coverage are hard family-freeze gates. "
            "Conflict-relevant access and fully-mapped transaction gas are diagnostics; selector/semantic/transaction readiness remains fail-closed after freeze."
        ),
        "next_gate": "after freeze, prepare-native recomputes selector semantic, transaction, contention, and implementation readiness",
    }
    atomic(ns.output, report)
    lines = [
        "Vegeta S4 family-review freeze gate",
        "",
        f"ready to freeze: {'PASS' if ready else 'FAIL'}",
        f"internal frozen-corpus integrity: {metrics['internal_corpus_integrity']}",
        f"conflict coverage: {100*metrics['conflict_coverage']:.2f}% (target {100*ns.min_conflict:.2f}%)",
        f"median conflict-bearing block coverage: {100*metrics['median_block_conflict_coverage']:.2f}% (target {100*ns.min_median_block:.2f}%)",
        f"all storage-access coverage: {100*metrics['storage_access_coverage']:.2f}% (target {100*ns.min_storage_access:.2f}%)",
        f"conflict-relevant storage-access coverage: {100*metrics['conflict_relevant_access_coverage']:.2f}% (diagnostic; reference {100*ns.min_conflict_relevant_access:.2f}%)",
        f"state-owner occurrence coverage: {100*metrics['state_owner_occurrence_coverage']:.2f}% (diagnostic)",
        f"fully mapped source-state gas: {100*metrics['fully_mapped_source_state_gas_coverage_diagnostic']:.2f}% (conservative diagnostic; not a gate)",
        f"fully mapped source-state transactions: {100*metrics['fully_mapped_source_state_transaction_coverage_diagnostic']:.2f}% (conservative diagnostic; not a gate)",
        f"profile mappings: {metrics['profile_mappings']}",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    if not ready and not ns.allow_low:
        raise SystemExit("S4 reviewed family map is below dual publication/freeze gates; continue with families that close the remaining storage-access and conflict deficits")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
