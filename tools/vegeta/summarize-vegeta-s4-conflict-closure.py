#!/usr/bin/env python3
"""Summarize exact S4 conflict-closure progress after a review batch.

The report intentionally treats source conflict coverage, median conflict-bearing-block coverage,
and corpus integrity as the family-freeze gates. Storage-access and strict transaction-gas metrics
remain visible diagnostics but do not decide freeze readiness.
"""
from __future__ import annotations

import argparse
import json
import math
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
    ap.add_argument("--coverage", type=Path, required=True)
    ap.add_argument("--readiness", type=Path, required=True)
    ap.add_argument("--decisions", type=Path, required=True)
    ap.add_argument("--min-conflict", type=float, default=0.95)
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ns = ap.parse_args()
    if not 0 < ns.min_conflict <= 1:
        raise SystemExit("--min-conflict must be in (0,1]")
    if ns.top <= 0:
        raise SystemExit("--top must be positive")

    coverage = read(ns.coverage)
    readiness = read(ns.readiness)
    decisions = read(ns.decisions)
    source = coverage.get("source_conflict_coverage") or {}
    balanced = coverage.get("block_balanced_conflict_coverage") or {}
    relevant = coverage.get("conflict_relevant_storage_access_coverage") or {}
    all_storage = coverage.get("storage_access_coverage") or {}
    gas = coverage.get("gas_weighted_family_coverage") or {}

    total_pairs = int(source.get("total_unique_conflict_pairs") or 0)
    covered_pairs = int(source.get("selected_family_unique_conflict_pairs") or 0)
    target_pairs = math.ceil(ns.min_conflict * total_pairs) if total_pairs else 0
    remaining_pairs = max(target_pairs - covered_pairs, 0)
    reviewed_rows = [r for r in decisions.get("decisions") or [] if str(r.get("review_status") or "").lower() == "reviewed"]
    pending_rows = [r for r in decisions.get("decisions") or [] if str(r.get("review_status") or "pending").lower() != "reviewed"]

    top = []
    for row in (coverage.get("top_unmapped_conflict_owners") or [])[: ns.top]:
        top.append({
            "address": row.get("address"),
            "owner_pair_attributions": int(row.get("owner_pair_attributions") or 0),
            "access_records": int(row.get("access_records") or 0),
            "gas_attributions": int(row.get("gas_attributions") or 0),
        })

    report = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "freeze_policy": "conflict-only family gate: corpus integrity + >=95% unique source conflict pairs + >=80% median conflict-bearing-block coverage; access/gas metrics are diagnostics",
        "ready_to_freeze_family_map": bool(readiness.get("ready_to_freeze_family_map")),
        "conflict": {
            "covered_pairs": covered_pairs,
            "total_pairs": total_pairs,
            "coverage": float(source.get("coverage") or 0.0),
            "target": ns.min_conflict,
            "target_pairs": target_pairs,
            "remaining_pairs_to_target": remaining_pairs,
            "median_conflict_bearing_block_coverage": float(balanced.get("median_coverage") or 0.0),
        },
        "diagnostics": {
            "conflict_relevant_storage_access_coverage": float(relevant.get("access_record_coverage") or 0.0),
            "all_storage_access_coverage": float(all_storage.get("access_record_coverage") or 0.0),
            "state_owner_occurrence_coverage": float(all_storage.get("state_owner_occurrence_coverage") or 0.0),
            "fully_mapped_state_gas_coverage": float(gas.get("fully_selected_family_state_gas_coverage") or 0.0),
            "fully_mapped_state_transaction_coverage": float(gas.get("fully_selected_family_state_transaction_coverage") or 0.0),
        },
        "review_workspace": {
            "reviewed_rows": len(reviewed_rows),
            "pending_rows": len(pending_rows),
            "pending_runtime_families": [str(r.get("runtime_code_family") or "") for r in pending_rows],
        },
        "top_unmapped_conflict_owners": top,
    }
    atomic(ns.output, report)

    lines = [
        "Vegeta S4 exact conflict-closure report",
        "",
        f"family freeze gate: {'PASS' if report['ready_to_freeze_family_map'] else 'FAIL'}",
        f"conflict pairs: {covered_pairs}/{total_pairs} ({100*report['conflict']['coverage']:.2f}%) target={100*ns.min_conflict:.2f}%",
        f"remaining unique conflict pairs to target: {remaining_pairs}",
        f"median conflict-bearing block coverage: {100*report['conflict']['median_conflict_bearing_block_coverage']:.2f}%",
        "",
        "Diagnostics (reported, not family-freeze gates):",
        f"  conflict-relevant storage-access coverage: {100*report['diagnostics']['conflict_relevant_storage_access_coverage']:.2f}%",
        f"  all storage-access coverage: {100*report['diagnostics']['all_storage_access_coverage']:.2f}%",
        f"  state-owner occurrence coverage: {100*report['diagnostics']['state_owner_occurrence_coverage']:.2f}%",
        f"  fully mapped source-state gas: {100*report['diagnostics']['fully_mapped_state_gas_coverage']:.2f}%",
        f"  fully mapped source-state transactions: {100*report['diagnostics']['fully_mapped_state_transaction_coverage']:.2f}%",
        "",
        f"review workspace: reviewed={len(reviewed_rows)} pending={len(pending_rows)}",
        "",
        "Top remaining unmapped conflict owners:",
    ]
    for row in top:
        lines.append(
            f"  {row['address']} pairs={row['owner_pair_attributions']} accesses={row['access_records']} gas_attr={row['gas_attributions']}"
        )
    if remaining_pairs == 0:
        lines += ["", "Conflict target is closed. If the family freeze gate also passes, freeze the reviewed map; selector/semantic readiness is still enforced during native preparation."]
    else:
        lines += ["", "Conflict target is not closed. Review only the highest-value remaining families; do not chase access-only long-tail state for the family freeze gate."]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
