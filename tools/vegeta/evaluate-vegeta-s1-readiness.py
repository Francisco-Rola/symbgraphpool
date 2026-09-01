#!/usr/bin/env python3
"""Evaluate explicit Vegeta S1 readiness profiles from frozen local coverage reports.

This tool keeps scheduler/dependency fidelity distinct from general semantic-replay
coverage.  It never changes the underlying coverage measurements; it only applies
named thresholds to already-generated reports.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

PROFILES = ("scheduler-fidelity", "semantic-replay")


def load_json(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        raise SystemExit(f"expected JSON object: {path}")
    return data


def require_float(value: Any, label: str) -> float:
    try:
        return float(value)
    except (TypeError, ValueError) as exc:
        raise SystemExit(f"missing/invalid readiness metric {label}: {value!r}") from exc


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--translation-coverage", type=Path, required=True)
    ap.add_argument("--semantic-conflict-coverage", type=Path, required=True)
    ap.add_argument("--transaction-deficit", type=Path, required=True)
    ap.add_argument("--profile", choices=PROFILES, default="semantic-replay")
    ap.add_argument("--min-conflict", type=float, default=0.95)
    ap.add_argument("--min-median-block", type=float, default=0.80)
    ap.add_argument("--min-semantic-tx", type=float, default=0.80)
    ap.add_argument("--min-contention-tx", type=float, default=0.80)
    ap.add_argument("--min-reviewed-state-frame", type=float, default=0.00)
    ap.add_argument("--allow-low", action="store_true")
    ap.add_argument("--output", type=Path)
    ap.add_argument("--text-output", type=Path)
    ns = ap.parse_args()

    translation = load_json(ns.translation_coverage)
    semantic = load_json(ns.semantic_conflict_coverage)
    deficit = load_json(ns.transaction_deficit)

    conflict = require_float(semantic.get("coverage"), "reviewed state-touch conflict coverage")
    median = require_float((semantic.get("block_balanced") or {}).get("median_coverage"), "median conflict-bearing block coverage")
    frame_cov = require_float((translation.get("calls") or {}).get("reviewed_state_touch_frame_coverage"), "reviewed state-touch frame coverage")
    implementation_ready = bool((translation.get("implementation_readiness") or {}).get("native_execution_ready"))

    denoms = deficit.get("denominators") or {}
    all_row = denoms.get("all_source_transactions") or {}
    contention_row = denoms.get("source_conflict_participating_transactions") or {}
    all_tx_cov = require_float(all_row.get("successful_reviewed_state_coverage"), "all-transaction successful reviewed-state coverage")
    contention_cov = require_float(contention_row.get("successful_reviewed_state_coverage"), "conflict-participant successful reviewed-state coverage")

    common = {
        "reviewed_state_touch_conflict_coverage": {
            "value": conflict,
            "minimum": ns.min_conflict,
            "pass": conflict >= ns.min_conflict,
        },
        "median_conflict_block_coverage": {
            "value": median,
            "minimum": ns.min_median_block,
            "pass": median >= ns.min_median_block,
        },
        "reviewed_state_touch_frame_coverage": {
            "value": frame_cov,
            "minimum": ns.min_reviewed_state_frame,
            "pass": frame_cov >= ns.min_reviewed_state_frame,
            "diagnostic": ns.min_reviewed_state_frame <= 0,
        },
        "native_execution_implementation_ready": {
            "value": implementation_ready,
            "pass": implementation_ready,
        },
    }
    profile_specific = {
        "scheduler-fidelity": {
            "successful_reviewed_conflict_participant_tx_coverage": {
                "value": contention_cov,
                "minimum": ns.min_contention_tx,
                "pass": contention_cov >= ns.min_contention_tx,
                "numerator": int(contention_row.get("successful_reviewed_state_transactions") or 0),
                "denominator": int(contention_row.get("transactions") or 0),
            }
        },
        "semantic-replay": {
            "successful_reviewed_all_tx_coverage": {
                "value": all_tx_cov,
                "minimum": ns.min_semantic_tx,
                "pass": all_tx_cov >= ns.min_semantic_tx,
                "numerator": int(all_row.get("successful_reviewed_state_transactions") or 0),
                "denominator": int(all_row.get("transactions") or 0),
            }
        },
    }

    def profile_pass(profile: str) -> bool:
        enforced_common = (row for row in common.values() if not row.get("diagnostic", False))
        return all(bool(row["pass"]) for row in enforced_common) and all(
            bool(row["pass"]) for row in profile_specific[profile].values()
        )

    report = {
        "schema_version": 1,
        "dataset": "vegeta-s1",
        "selected_profile": ns.profile,
        "selected_profile_ready": profile_pass(ns.profile),
        "allow_low_override": bool(ns.allow_low),
        "definition": {
            "scheduler-fidelity": "dependency/contention benchmark readiness: reviewed state-touch conflict structure, block-balanced conflict coverage, executable reviewed families, and successful reviewed semantics among source conflict-participating transactions",
            "semantic-replay": "stronger general-replay readiness: the common dependency gates plus successful reviewed-state semantics across all retained source transactions",
            "non_substitution": "scheduler-fidelity readiness does not imply general Ethereum semantic equivalence; both profiles remain reported",
        },
        "common_gates": common,
        "profiles": {
            profile: {
                "ready": profile_pass(profile),
                "gates": profile_specific[profile],
            }
            for profile in PROFILES
        },
    }

    lines = [
        "Vegeta S1 native readiness profiles",
        "",
        f"selected profile: {ns.profile}",
        f"selected profile ready: {'PASS' if report['selected_profile_ready'] else 'FAIL'}",
        "",
        "Common gates:",
        f"  reviewed state-touch conflicts: {conflict:.2%} >= {ns.min_conflict:.2%}  {'PASS' if common['reviewed_state_touch_conflict_coverage']['pass'] else 'FAIL'}",
        f"  median conflict-bearing block:  {median:.2%} >= {ns.min_median_block:.2%}  {'PASS' if common['median_conflict_block_coverage']['pass'] else 'FAIL'}",
        f"  native implementation ready:   {implementation_ready}  {'PASS' if implementation_ready else 'FAIL'}",
        (
            f"  reviewed state-touch frames:    {frame_cov:.2%}  DIAGNOSTIC (not gated)"
            if common["reviewed_state_touch_frame_coverage"]["diagnostic"]
            else f"  reviewed state-touch frames:    {frame_cov:.2%} >= {ns.min_reviewed_state_frame:.2%}  {'PASS' if common['reviewed_state_touch_frame_coverage']['pass'] else 'FAIL'}"
        ),
        "",
        "Profile-specific transaction gates:",
        f"  scheduler-fidelity conflict participants: {contention_cov:.2%} >= {ns.min_contention_tx:.2%}  {'PASS' if profile_specific['scheduler-fidelity']['successful_reviewed_conflict_participant_tx_coverage']['pass'] else 'FAIL'}",
        f"  semantic-replay all transactions:          {all_tx_cov:.2%} >= {ns.min_semantic_tx:.2%}  {'PASS' if profile_specific['semantic-replay']['successful_reviewed_all_tx_coverage']['pass'] else 'FAIL'}",
        "",
        "Interpretation: scheduler-fidelity is the readiness profile for the Vegeta S1 contention/scheduler benchmark.",
        "It does not replace or hide the all-transaction semantic-replay metric, which remains a separate stronger claim.",
    ]
    text = "\n".join(lines) + "\n"

    if ns.output:
        ns.output.parent.mkdir(parents=True, exist_ok=True)
        ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    if ns.text_output:
        ns.text_output.parent.mkdir(parents=True, exist_ok=True)
        ns.text_output.write_text(text)
    print(text, end="")

    if not report["selected_profile_ready"] and not ns.allow_low:
        raise SystemExit(
            f"S1 readiness profile {ns.profile!r} is below threshold. "
            "Do not relabel one profile as the other; use --allow-low only for diagnostics."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
