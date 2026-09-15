#!/usr/bin/env python3
"""Build the next S4 scheduler-fidelity conflict-closure review scaffold.

This tool is deliberately non-executable: it chooses the projected minimum conflict-first
prefix needed to cross the hard conflict target and emits a separate access-heavy tail for
later semantic-surface work.  Every executable mapping still requires explicit human review,
source/interface evidence, a reviewed native family, and a concrete mapping basis.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def family_key(row: dict) -> str:
    return str(row.get("runtime_code_family") or row.get("blocker_id") or row.get("address") or "")


def compact_step(row: dict, queue_by_family: dict[str, list[dict]]) -> dict:
    family = str(row.get("runtime_code_family") or "")
    evidence_rows = queue_by_family.get(family, [])
    addresses: list[str] = []
    for address in row.get("owners") or []:
        address = str(address).lower()
        if address and address not in addresses:
            addresses.append(address)
    for q in evidence_rows:
        address = str(q.get("address") or "").lower()
        if address and address not in addresses:
            addresses.append(address)
    selectors = []
    for q in evidence_rows:
        for kind in ("direct_selectors", "call_frame_selectors"):
            for item in q.get(kind) or []:
                entry = {"kind": kind, **item}
                if entry not in selectors:
                    selectors.append(entry)
    return {
        "blocker_id": row.get("blocker_id"),
        "runtime_code_family": family or None,
        "address": row.get("address"),
        "owner_addresses": addresses[:100],
        "owner_count": int(row.get("owner_count", len(addresses)) or len(addresses)),
        "identity_hint": row.get("identity_hint"),
        "suggested_native_family": row.get("suggested_native_family"),
        "implementation_disposition": row.get("implementation_disposition"),
        "semantic_notes": row.get("semantic_notes"),
        "projected_new_storage_access_records": int(row.get("newly_covered_storage_access_records", 0) or 0),
        "projected_new_conflict_pairs": int(row.get("newly_covered_conflict_pairs", 0) or 0),
        "projected_remaining_storage_access_deficit_records": int(row.get("remaining_storage_access_deficit_records", 0) or 0),
        "projected_remaining_conflict_deficit_pairs": int(row.get("remaining_conflict_deficit_pairs", 0) or 0),
        "projected_cumulative_storage_access_coverage": float(row.get("cumulative_projected_storage_access_coverage", 0.0) or 0.0),
        "projected_cumulative_conflict_coverage": float(row.get("cumulative_projected_conflict_coverage", 0.0) or 0.0),
        "strict_gas_unlocked_diagnostic": int(row.get("newly_unlocked_gas", 0) or 0),
        "observed_selectors": selectors[:40],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--plan", type=Path, required=True)
    ap.add_argument("--review-queue", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--decisions-draft", type=Path, required=True)
    ap.add_argument("--access-tail-top", type=int, default=40)
    ap.add_argument("--starting-priority", type=int, default=17)
    ap.add_argument("--force-decisions-draft", action="store_true")
    ns = ap.parse_args()

    plan = read_json(ns.plan)
    queue = read_json(ns.review_queue)
    workspace = read_json(ns.workspace_decisions)
    if plan.get("dataset") != "vegeta-s4" or queue.get("dataset") != "vegeta-s4" or workspace.get("dataset") != "vegeta-s4":
        raise SystemExit("all fifth-batch scaffold inputs must have dataset=vegeta-s4")

    queue_by_family: dict[str, list[dict]] = {}
    for row in queue.get("review_queue") or []:
        family = str(row.get("runtime_code_family") or "")
        if family:
            queue_by_family.setdefault(family, []).append(row)

    conflict_steps = list((plan.get("conflict_first_plan") or {}).get("steps") or [])
    if not conflict_steps:
        raise SystemExit("planner emitted no conflict-first steps")

    workspace_by_family = {
        str(row.get("runtime_code_family") or ""): row
        for row in workspace.get("decisions") or []
        if row.get("runtime_code_family")
    }
    explicitly_non_executable = {
        family
        for family, row in workspace_by_family.items()
        if str(row.get("review_status") or "pending").lower() in {"unresolved", "deferred", "rejected"}
    }

    # When a prior human review explicitly leaves a family unresolved/deferred/rejected, do not
    # immediately put it back at the head of the next review batch. Planner marginal gains after a
    # skipped family are conservative lower bounds (the skipped family can only have overlapped with
    # later candidates), so recompute a conservative cumulative conflict count from the current exact
    # count plus the retained step gains. This lets a later, well-evidenced family close the gate
    # without pretending the unresolved row was selected.
    total_conflict_pairs = int(plan.get("total_conflict_pairs", 0) or 0)
    current_conflict_pairs = int(plan.get("current_covered_conflict_pairs", 0) or 0)
    target_conflict_pairs = int(plan.get("target_conflict_pairs", 0) or 0)
    if not total_conflict_pairs or not target_conflict_pairs:
        raise SystemExit("planner output lacks exact conflict-pair counts required for fail-closed shortlist filtering")

    conservative_pairs = current_conflict_pairs
    conflict_shortlist: list[dict] = []
    skipped_non_executable: list[str] = []
    for row in conflict_steps:
        family = family_key(row)
        if family in explicitly_non_executable:
            skipped_non_executable.append(family)
            continue
        compact = compact_step(row, queue_by_family)
        conservative_pairs = min(total_conflict_pairs, conservative_pairs + int(compact["projected_new_conflict_pairs"]))
        compact["projected_cumulative_conflict_coverage"] = conservative_pairs / total_conflict_pairs
        compact["projected_remaining_conflict_deficit_pairs"] = max(target_conflict_pairs - conservative_pairs, 0)
        conflict_shortlist.append(compact)
        if conservative_pairs >= target_conflict_pairs:
            break
    if not conflict_shortlist or conservative_pairs < target_conflict_pairs:
        raise SystemExit(
            "wide planner did not conservatively project closure of the conflict gate after excluding explicitly non-executable reviews; "
            "increase VEGETA_S4_COVERAGE_PLAN_MAX_STEPS or revisit a deferred review"
        )

    selected = {family_key(r) for r in conflict_shortlist}
    access_tail: list[dict] = []
    for row in list((plan.get("access_first_plan") or {}).get("steps") or []):
        if family_key(row) in selected or family_key(row) in explicitly_non_executable:
            continue
        access_tail.append(compact_step(row, queue_by_family))
        if len(access_tail) >= ns.access_tail_top:
            break

    existing_reviewed = {
        str(row.get("runtime_code_family") or "")
        for row in workspace.get("decisions") or []
        if str(row.get("review_status") or "pending").lower() == "reviewed"
    }
    duplicate = sorted(family_key(r) for r in conflict_shortlist if family_key(r) in existing_reviewed)
    if duplicate:
        raise SystemExit("conflict shortlist unexpectedly contains already-reviewed families: " + ", ".join(duplicate))

    out = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": (
            "Projected fifth review batch: minimum conflict-first planner prefix needed to cross the 95% conflict gate, "
            "plus an independent access-heavy tail. This is a review scaffold only; no row is an executable mapping."
        ),
        "planner_source": str(ns.plan),
        "current_storage_access_coverage": float(plan.get("current_storage_access_coverage", 0.0) or 0.0),
        "current_conflict_coverage": float(plan.get("current_conflict_coverage", 0.0) or 0.0),
        "target_storage_access_coverage": float(plan.get("target_storage_access_coverage", 0.90) or 0.90),
        "target_conflict_coverage": float(plan.get("target_conflict_coverage", 0.95) or 0.95),
        "remaining_storage_access_deficit_records": int(plan.get("remaining_storage_access_deficit_records", 0) or 0),
        "remaining_conflict_deficit_pairs": int(plan.get("remaining_conflict_deficit_pairs", 0) or 0),
        "projected_conflict_closure_review_count": len(conflict_shortlist),
        "explicitly_non_executable_families_skipped": skipped_non_executable,
        "projected_conflict_closure_shortlist": conflict_shortlist,
        "access_heavy_followup": access_tail,
        "review_requirements": [
            "identify the deployed contract/family from reproducible public source or verified interface evidence",
            "record exercised selector/call-frame evidence and storage semantics relevant to source conflicts",
            "choose an already implemented native family only when the state dependency semantics match",
            "otherwise add a distinct conservative native dependency family before marking reviewed",
            "fill reviewed_native_family, mapping_basis, review_conclusion, and evidence_sources",
            "rerun exact scheduler-fidelity family coverage after application; planner gains are projections, not credited coverage",
        ],
    }
    atomic(ns.output, out)

    preserve_existing_draft = ns.decisions_draft.exists() and not ns.force_decisions_draft
    decisions = []
    for idx, row in enumerate(conflict_shortlist, start=ns.starting_priority):
        decisions.append({
            "priority": idx,
            "address": (row.get("owner_addresses") or [row.get("address") or ""])[0],
            "runtime_code_family": row.get("runtime_code_family"),
            "identity_hint": row.get("identity_hint"),
            "suggested_native_family": row.get("suggested_native_family"),
            "semantic_notes": row.get("semantic_notes"),
            "projected_new_storage_access_records": row.get("projected_new_storage_access_records"),
            "projected_new_conflict_pairs": row.get("projected_new_conflict_pairs"),
            "review_status": "pending",
            "reviewed_native_family": None,
            "mapping_basis": "",
            "review_conclusion": "",
            "evidence_sources": [],
        })
    draft = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": (
            "Human-editable fifth S4 review decisions generated from the exact conflict-first planner prefix. "
            "Pending rows are non-executable; do not mark reviewed without concrete semantic evidence."
        ),
        "source_scaffold": str(ns.output),
        "decisions": decisions,
    }
    if preserve_existing_draft:
        print(f"preserving existing human-editable fifth-batch decisions draft: {ns.decisions_draft}")
    else:
        atomic(ns.decisions_draft, draft)

    lines = [
        "Vegeta S4 fifth-batch review scaffold",
        "",
        f"current all-storage coverage: {100*out['current_storage_access_coverage']:.2f}%",
        f"current conflict coverage: {100*out['current_conflict_coverage']:.2f}%",
        f"remaining all-storage deficit: {out['remaining_storage_access_deficit_records']} records",
        f"remaining conflict deficit: {out['remaining_conflict_deficit_pairs']} pairs",
        f"projected reviews to cross conflict target: {len(conflict_shortlist)}",
        f"explicitly non-executable prior reviews skipped: {len(skipped_non_executable)}",
        "",
        "Projected conflict-closure shortlist (REVIEW REQUIRED; not executable mappings):",
    ]
    for i, row in enumerate(conflict_shortlist, start=1):
        label = row.get("identity_hint") or row.get("runtime_code_family") or row.get("blocker_id")
        lines.append(
            f"  {i:2d}. {label}: access+={row['projected_new_storage_access_records']} "
            f"conflict+={row['projected_new_conflict_pairs']} projected-conflict={100*row['projected_cumulative_conflict_coverage']:.2f}%"
        )
    lines += ["", f"Access-heavy follow-up (top {len(access_tail)} excluding the conflict shortlist):"]
    for i, row in enumerate(access_tail, start=1):
        label = row.get("identity_hint") or row.get("runtime_code_family") or row.get("blocker_id")
        lines.append(
            f"  {i:2d}. {label}: access+={row['projected_new_storage_access_records']} conflict+={row['projected_new_conflict_pairs']}"
        )
    lines += [
        "",
        "Next: edit the decisions draft. Every row must have review_status=reviewed, reviewed_native_family,",
        "mapping_basis, review_conclusion, and at least one evidence_sources entry before the fifth-batch apply runner will accept it.",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"fifth-batch projected conflict reviews: {len(conflict_shortlist)}")
    print(f"access-tail candidates: {len(access_tail)}")
    print(f"wrote {ns.output}")
    print(f"wrote {ns.text_output}")
    if preserve_existing_draft:
        print(f"preserved {ns.decisions_draft}")
    else:
        print(f"wrote {ns.decisions_draft}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
