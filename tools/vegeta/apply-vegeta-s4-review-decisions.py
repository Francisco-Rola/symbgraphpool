#!/usr/bin/env python3
"""Apply only explicitly reviewed S4 family decisions to a candidate family-map draft.

The input decisions file is designed to be edited by a human.  Pending rows are ignored.  A row
marked ``reviewed`` must name a native family and contain a non-empty mapping basis.  The output
remains ``candidate_only``; freezing is a separate gated operation after coverage is recomputed.
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
    ap.add_argument("--base-map", type=Path, required=True)
    ap.add_argument("--decisions", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ns = ap.parse_args()
    fmap = read(ns.base_map)
    decisions = read(ns.decisions)
    if fmap.get("dataset") != "vegeta-s4" or decisions.get("dataset") != "vegeta-s4":
        raise SystemExit("S4 review inputs must both have dataset=vegeta-s4")
    native = fmap.get("native_code_families") or {}
    mappings = [dict(row) for row in (fmap.get("profile_mappings") or [])]
    by_family = {str(row.get("ethereum_profile_family")): row for row in mappings if row.get("ethereum_profile_family")}
    applied = []
    pending = []
    max_rank = max((int(row.get("rank", 0) or 0) for row in mappings), default=0)

    for row in sorted(decisions.get("decisions") or [], key=lambda r: (int(r.get("priority", 9999)), str(r.get("address", "")))):
        status = str(row.get("review_status") or "pending").lower()
        if status != "reviewed":
            pending.append(str(row.get("runtime_code_family") or ""))
            continue
        family = str(row.get("runtime_code_family") or "")
        target = str(row.get("reviewed_native_family") or "")
        basis = str(row.get("mapping_basis") or "").strip()
        if not family or not target or not basis:
            raise SystemExit(f"reviewed decision must provide runtime_code_family, reviewed_native_family, and mapping_basis: {row}")
        if target not in native:
            raise SystemExit(f"reviewed decision references unknown native family {target!r}; add its implementation/profile to the draft family map first")
        max_rank += 1
        mapped = {
            "ethereum_contract_identifier": str(row.get("identity_hint") or family),
            "ethereum_profile_family": family,
            "mapping_basis": basis,
            "native_code_family": target,
            "rank": max_rank,
            "s4_family_extension": True,
            "storage_owner_scope": [str(row.get("address")).lower()] if row.get("address") else [],
            "s4_review_priority": int(row.get("priority", 9999)),
            "s4_semantic_notes": str(row.get("semantic_notes") or ""),
        }
        if family in by_family:
            old = by_family[family]
            old.update(mapped)
        else:
            mappings.append(mapped)
            by_family[family] = mapped
        applied.append(family)

    fmap["profile_mappings"] = mappings
    fmap["selected_profile_families"] = len(mappings)
    fmap["expected_profile_mappings"] = len(mappings)
    fmap["candidate_only"] = True
    fmap["status"] = "s4-reviewed-draft-pending-coverage-and-freeze-gates"
    fmap["review_status"] = f"review decisions applied={len(applied)} pending={len(pending)}; recompute coverage before freeze"
    fmap["s4_review_decisions_source"] = str(ns.decisions)
    fmap["s4_reviewed_runtime_families"] = applied
    fmap["s4_pending_seed_runtime_families"] = pending
    atomic(ns.output, fmap)
    print(f"S4 review decisions applied: {len(applied)}; pending seed rows: {len(pending)}")
    print(f"wrote {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
