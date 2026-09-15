#!/usr/bin/env python3
"""Install the checked-in S4 fourth conflict-closure batch into the review workspace.

The fourth batch promotes the remaining P12-P16 manual-review scaffold only after a checked-in
public-source evidence record exists for every reviewed runtime family. New conservative native
family aliases are merged into the persistent review base before decisions are validated. Existing
reviewed decisions are never replaced. The resulting family map remains candidate-only until the
normal exact family freeze gate passes and the user explicitly attests the review.
"""
from __future__ import annotations

import argparse
import hashlib
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
    ap.add_argument("--review-base", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ap.add_argument("--family-extension", type=Path, required=True)
    ap.add_argument("--fourth-batch", type=Path, required=True)
    ap.add_argument("--review-evidence", type=Path, required=True)
    ap.add_argument("--pending-output", type=Path, required=True)
    ns = ap.parse_args()

    base = read(ns.review_base)
    workspace = read(ns.workspace_decisions)
    ext = read(ns.family_extension)
    batch = read(ns.fourth_batch)
    evidence = read(ns.review_evidence)
    for label, doc in (
        ("review base", base),
        ("workspace decisions", workspace),
        ("family extension", ext),
        ("fourth batch", batch),
        ("review evidence", evidence),
    ):
        if doc.get("dataset") != "vegeta-s4":
            raise SystemExit(f"{label} must have dataset=vegeta-s4")

    evidence_by_id = {
        str(row.get("evidence_id") or ""): row
        for row in (evidence.get("records") or [])
        if row.get("evidence_id")
    }

    native = dict(base.get("native_code_families") or {})
    added_native = []
    for name, config in sorted((ext.get("native_code_families") or {}).items()):
        if name in native and native[name] != config:
            raise SystemExit(f"refusing to replace a different existing native family definition: {name}")
        if name not in native:
            added_native.append(name)
        native[name] = config
    base["native_code_families"] = native
    base["expected_native_code_families"] = len(native)
    base["candidate_only"] = True
    base["s4_fourth_batch_native_extension"] = str(ns.family_extension)
    base["s4_fourth_batch_review_evidence"] = str(ns.review_evidence)
    base["s4_fourth_batch_review_evidence_sha256"] = hashlib.sha256(ns.review_evidence.read_bytes()).hexdigest()
    atomic(ns.review_base, base)

    current = [dict(row) for row in (workspace.get("decisions") or [])]
    index = {
        str(row.get("runtime_code_family") or ""): i
        for i, row in enumerate(current)
        if row.get("runtime_code_family")
    }
    promoted = []
    appended = []

    rows = sorted(
        batch.get("decisions") or [],
        key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", ""))),
    )
    if not rows or any(str(r.get("review_status") or "").lower() != "reviewed" for r in rows):
        raise SystemExit("fourth-batch file must contain only reviewed rows")
    priorities = [int(r.get("priority", -1)) for r in rows]
    if priorities != [12, 13, 14, 15, 16]:
        raise SystemExit(f"fourth batch must be exactly the reviewed P12-P16 scaffold; got priorities={priorities}")

    for row0 in rows:
        row = dict(row0)
        family = str(row.get("runtime_code_family") or "")
        target = str(row.get("reviewed_native_family") or "")
        basis = str(row.get("mapping_basis") or "").strip()
        evidence_id = str(row.get("evidence_id") or "")
        ev = evidence_by_id.get(evidence_id)
        if not family:
            raise SystemExit(f"fourth-batch row lacks runtime_code_family: {row}")
        if not target or target not in native:
            raise SystemExit(f"fourth-batch family {family} references unknown native family {target!r}")
        if not basis:
            raise SystemExit(f"fourth-batch family {family} lacks mapping_basis")
        if not ev or str(ev.get("runtime_code_family") or "") != family:
            raise SystemExit(f"fourth-batch family {family} lacks matching review evidence {evidence_id!r}")
        if not str(ev.get("review_conclusion") or "").strip() or not (ev.get("sources") or []):
            raise SystemExit(f"review evidence {evidence_id!r} is incomplete")

        if family not in index:
            current.append(row)
            index[family] = len(current) - 1
            appended.append(family)
            continue

        i = index[family]
        old = current[i]
        old_status = str(old.get("review_status") or "pending").lower()
        if old == row:
            continue
        if old_status == "pending" and not old.get("reviewed_native_family"):
            current[i] = row
            promoted.append(family)
            continue
        raise SystemExit(
            f"workspace already contains a different reviewed decision for {family}; "
            "refusing to replace human-reviewed state automatically"
        )

    workspace["schema_version"] = max(int(workspace.get("schema_version", 1) or 1), 1)
    workspace["dataset"] = "vegeta-s4"
    workspace["purpose"] = (
        "S4 cumulative human-review workspace. Checked-in reviewed family decisions are executable only "
        "in a candidate draft; the exact scheduler-fidelity family conflict/median gates and later selector/native gates "
        "remain fail-closed."
    )
    workspace["decisions"] = sorted(
        current,
        key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", ""))),
    )
    workspace["installed_fourth_batch_source"] = str(ns.fourth_batch)
    workspace["installed_fourth_batch_native_extension"] = str(ns.family_extension)
    workspace["installed_fourth_batch_review_evidence"] = str(ns.review_evidence)
    workspace["installed_fourth_batch_review_evidence_sha256"] = hashlib.sha256(ns.review_evidence.read_bytes()).hexdigest()
    workspace["installed_fourth_batch_promoted_families"] = promoted
    workspace["installed_fourth_batch_appended_families"] = appended
    atomic(ns.workspace_decisions, workspace)

    pending = [
        dict(r)
        for r in workspace["decisions"]
        if str(r.get("review_status") or "pending").lower() != "reviewed"
    ]
    pending_doc = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": "Remaining explicit S4 manual-review scaffold after the fourth conflict-closure batch. These rows are not executable mappings.",
        "source": str(ns.fourth_batch),
        "review_evidence": str(ns.review_evidence),
        "pending_count": len(pending),
        "decisions": pending,
    }
    atomic(ns.pending_output, pending_doc)

    print(f"fourth-batch native aliases added: {len(added_native)}")
    print(f"fourth-batch pending rows promoted: {len(promoted)}")
    print(f"fourth-batch reviewed rows appended: {len(appended)}")
    print(f"remaining explicit pending rows: {len(pending)}")
    print(f"workspace decisions: {ns.workspace_decisions}")
    print(f"review evidence:     {ns.review_evidence}")
    print(f"pending scaffold:    {ns.pending_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
