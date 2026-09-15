#!/usr/bin/env python3
"""Evaluate S1-analogous Vegeta S4 readiness profiles.

S4 is a contention/scheduler benchmark derived from real Ethereum traces.  As with S1,
scheduler-fidelity readiness is intentionally distinct from stronger general semantic
replay coverage.  All-source storage-access volume and strict gas completeness are
reported diagnostics, not scheduler-fidelity publication gates.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

PROFILES = ("scheduler-fidelity", "semantic-replay")


def load(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise SystemExit(f"expected JSON object: {path}")
    return data


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--family-coverage", type=Path, required=True)
    ap.add_argument("--translation-coverage", type=Path, required=True)
    ap.add_argument("--semantic-coverage", type=Path, required=True)
    ap.add_argument("--transaction-deficit", type=Path, required=True)
    ap.add_argument("--profile", choices=PROFILES, default="scheduler-fidelity")
    ap.add_argument("--min-conflict", type=float, default=0.95)
    ap.add_argument("--min-median-block", type=float, default=0.80)
    ap.add_argument(
        "--family-storage-access-reference", "--min-family-storage-access",
        dest="family_storage_access_reference", type=float, default=0.90,
        help="diagnostic all-source storage-access reference (not a readiness gate)",
    )
    ap.add_argument(
        "--min-family-conflict-relevant-access", type=float, default=0.90,
        help="diagnostic conflict-relevant access reference",
    )
    ap.add_argument("--min-semantic-tx", type=float, default=0.80)
    ap.add_argument("--min-contention-tx", type=float, default=0.80)
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
    denoms = deficit.get("denominators") or {}
    all_row = denoms.get("all_source_transactions") or {}
    state_row = denoms.get("source_storage_access_transactions") or {}
    contention_row = denoms.get("source_conflict_participating_transactions") or {}
    frame_cov = float((trans.get("calls") or {}).get("reviewed_state_touch_frame_coverage") or 0.0)
    metrics = {
        "family_conflict": float((fam.get("source_conflict_coverage") or {}).get("coverage", 0)),
        "family_median": float((fam.get("block_balanced_conflict_coverage") or {}).get("median_coverage") or 0),
        "family_storage_access_diagnostic": float(storage.get("access_record_coverage") or 0),
        "family_conflict_relevant_access_diagnostic": float(relevant_storage.get("access_record_coverage") or 0),
        "family_state_owner_occurrence_diagnostic": float(storage.get("state_owner_occurrence_coverage") or 0),
        "family_fully_mapped_state_gas_diagnostic": float(family_gas.get("fully_selected_family_state_gas_coverage") or 0),
        "semantic_conflict": float(sem.get("coverage", 0)),
        "semantic_median": float((sem.get("block_balanced") or {}).get("median_coverage") or 0),
        "reviewed_state_touch_frame_coverage_diagnostic": frame_cov,
        "semantic_tx": float(all_row.get("successful_reviewed_state_coverage") or 0),
        "semantic_state_gas_diagnostic": float(state_row.get("successful_reviewed_state_gas_coverage") or 0),
        "contention_tx": float(contention_row.get("successful_reviewed_state_coverage") or 0),
        "implementation_ready": bool((trans.get("implementation_readiness") or {}).get("native_execution_ready")),
    }

    common = {
        "family_conflict": {"value": metrics["family_conflict"], "minimum": ns.min_conflict, "pass": metrics["family_conflict"] >= ns.min_conflict},
        "family_median": {"value": metrics["family_median"], "minimum": ns.min_median_block, "pass": metrics["family_median"] >= ns.min_median_block},
        "semantic_conflict": {"value": metrics["semantic_conflict"], "minimum": ns.min_conflict, "pass": metrics["semantic_conflict"] >= ns.min_conflict},
        "semantic_median": {"value": metrics["semantic_median"], "minimum": ns.min_median_block, "pass": metrics["semantic_median"] >= ns.min_median_block},
        "implementation_ready": {"value": metrics["implementation_ready"], "pass": metrics["implementation_ready"]},
    }
    profile_specific = {
        "scheduler-fidelity": {
            "successful_reviewed_conflict_participant_tx_coverage": {
                "value": metrics["contention_tx"],
                "minimum": ns.min_contention_tx,
                "pass": metrics["contention_tx"] >= ns.min_contention_tx,
                "numerator": int(contention_row.get("successful_reviewed_state_transactions") or 0),
                "denominator": int(contention_row.get("transactions") or 0),
            }
        },
        "semantic-replay": {
            "successful_reviewed_all_tx_coverage": {
                "value": metrics["semantic_tx"],
                "minimum": ns.min_semantic_tx,
                "pass": metrics["semantic_tx"] >= ns.min_semantic_tx,
                "numerator": int(all_row.get("successful_reviewed_state_transactions") or 0),
                "denominator": int(all_row.get("transactions") or 0),
            }
        },
    }

    def profile_pass(profile: str) -> bool:
        return all(bool(row["pass"]) for row in common.values()) and all(
            bool(row["pass"]) for row in profile_specific[profile].values()
        )

    selected_ready = profile_pass(ns.profile)
    gates = {name: bool(row["pass"]) for name, row in common.items()}
    gates.update({name: bool(row["pass"]) for name, row in profile_specific[ns.profile].items()})
    report = {
        "schema_version": 5,
        "dataset": "vegeta-s4",
        "selected_profile": ns.profile,
        "selected_profile_ready": selected_ready,
        "ready": selected_ready,
        "allow_low_override": bool(ns.allow_low),
        "definition": {
            "scheduler-fidelity": "dependency/contention benchmark readiness: family and selector-reviewed conflict structure, block-balanced conflict coverage, executable reviewed families, and successful reviewed semantics among source conflict-participating transactions",
            "semantic-replay": "stronger general-replay readiness: the common dependency gates plus successful reviewed-state semantics across all retained source transactions",
            "non_substitution": "scheduler-fidelity readiness does not imply general Ethereum semantic equivalence; both profiles remain reported",
        },
        "metrics": metrics,
        "common_gates": common,
        "gates": gates,
        "profiles": {
            profile: {"ready": profile_pass(profile), "gates": profile_specific[profile]}
            for profile in PROFILES
        },
        "thresholds": {
            "conflict": ns.min_conflict,
            "median_block": ns.min_median_block,
            "semantic_tx": ns.min_semantic_tx,
            "contention_tx": ns.min_contention_tx,
        },
        "diagnostic_reference_thresholds": {
            "family_storage_access": ns.family_storage_access_reference,
            "family_conflict_relevant_access": ns.min_family_conflict_relevant_access,
            "fully_mapped_family_state_gas": ns.min_family_gas,
            "successful_reviewed_state_gas": ns.min_semantic_gas,
        },
        "diagnostic_note": (
            "All-source storage-access volume, conflict-relevant access, state-owner occurrence, reviewed frame coverage, and strict gas completeness are transparent diagnostics. "
            "They are not scheduler-fidelity publication gates, matching the S1 readiness model."
        ),
        "oracle_scope": "no exact SLOAD/SSTORE oracle; S4 is a real-trace translation/throughput workload",
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    lines = [
        "Vegeta S4 native readiness profiles",
        "",
        f"selected profile: {ns.profile}",
        f"selected profile ready: {'PASS' if selected_ready else 'FAIL'}",
        "",
        "Common gates:",
        f"  family conflict coverage:          {100*metrics['family_conflict']:.2f}% >= {100*ns.min_conflict:.2f}%  {'PASS' if common['family_conflict']['pass'] else 'FAIL'}",
        f"  family median conflict block:       {100*metrics['family_median']:.2f}% >= {100*ns.min_median_block:.2f}%  {'PASS' if common['family_median']['pass'] else 'FAIL'}",
        f"  selector-reviewed conflict coverage:{100*metrics['semantic_conflict']:.2f}% >= {100*ns.min_conflict:.2f}%  {'PASS' if common['semantic_conflict']['pass'] else 'FAIL'}",
        f"  selector-reviewed median block:     {100*metrics['semantic_median']:.2f}% >= {100*ns.min_median_block:.2f}%  {'PASS' if common['semantic_median']['pass'] else 'FAIL'}",
        f"  native implementation ready:        {metrics['implementation_ready']}  {'PASS' if common['implementation_ready']['pass'] else 'FAIL'}",
        "",
        "Profile-specific transaction gates:",
        f"  scheduler-fidelity conflict participants: {100*metrics['contention_tx']:.2f}% >= {100*ns.min_contention_tx:.2f}%  {'PASS' if profile_specific['scheduler-fidelity']['successful_reviewed_conflict_participant_tx_coverage']['pass'] else 'FAIL'}",
        f"  semantic-replay all transactions:          {100*metrics['semantic_tx']:.2f}% >= {100*ns.min_semantic_tx:.2f}%  {'PASS' if profile_specific['semantic-replay']['successful_reviewed_all_tx_coverage']['pass'] else 'FAIL'}",
        "",
        "Diagnostics (not readiness gates):",
        f"  family all storage-access coverage:        {100*metrics['family_storage_access_diagnostic']:.2f}% (reference {100*ns.family_storage_access_reference:.2f}%)",
        f"  family conflict-relevant access coverage:  {100*metrics['family_conflict_relevant_access_diagnostic']:.2f}%",
        f"  family state-owner occurrence coverage:    {100*metrics['family_state_owner_occurrence_diagnostic']:.2f}%",
        f"  reviewed state-touch frame coverage:       {100*metrics['reviewed_state_touch_frame_coverage_diagnostic']:.2f}%",
        f"  fully mapped source-state gas:             {100*metrics['family_fully_mapped_state_gas_diagnostic']:.2f}%",
        f"  successful reviewed source-state gas:      {100*metrics['semantic_state_gas_diagnostic']:.2f}%",
        "",
        "Interpretation: scheduler-fidelity is the publication profile for the S4 contention/scheduler benchmark.",
        "It does not replace or hide the stronger all-transaction semantic-replay profile.",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    if not selected_ready and not ns.allow_low:
        raise SystemExit(f"S4 readiness profile {ns.profile!r} is below threshold")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
