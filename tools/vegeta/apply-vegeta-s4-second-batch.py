#!/usr/bin/env python3
"""Merge the checked-in S4 second conflict-closure batch into the review workspace.

Reviewed rows are appended to the active review decisions; pending rows are retained as explicit
manual-review scaffolding.  No candidate is frozen here, and no pending row becomes executable.
The operation is idempotent by runtime-code family.
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
    ap.add_argument("--review-base", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ap.add_argument("--second-batch", type=Path, required=True)
    ap.add_argument("--pending-output", type=Path, required=True)
    ns = ap.parse_args()

    base = read(ns.review_base)
    workspace = read(ns.workspace_decisions)
    batch = read(ns.second_batch)
    for label, doc in (("review base", base), ("workspace decisions", workspace), ("second batch", batch)):
        if doc.get("dataset") != "vegeta-s4":
            raise SystemExit(f"{label} must have dataset=vegeta-s4")

    native = base.get("native_code_families") or {}
    current = [dict(row) for row in (workspace.get("decisions") or [])]
    by_family = {str(row.get("runtime_code_family") or ""): row for row in current if row.get("runtime_code_family")}

    reviewed = []
    pending = []
    for row in sorted(batch.get("decisions") or [], key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", "")))):
        family = str(row.get("runtime_code_family") or "")
        if not family:
            raise SystemExit(f"second-batch row lacks runtime_code_family: {row}")
        status = str(row.get("review_status") or "pending").lower()
        if status not in {"reviewed", "pending"}:
            raise SystemExit(f"unsupported second-batch review_status={status!r} for {family}")
        if status == "reviewed":
            target = str(row.get("reviewed_native_family") or "")
            basis = str(row.get("mapping_basis") or "").strip()
            if not target or target not in native:
                raise SystemExit(f"reviewed second-batch family {family} references unknown native family {target!r}")
            if not basis:
                raise SystemExit(f"reviewed second-batch family {family} lacks mapping_basis")
            reviewed.append(family)
        else:
            if row.get("reviewed_native_family"):
                raise SystemExit(f"pending second-batch family {family} must not set reviewed_native_family")
            pending.append(dict(row))

        if family in by_family:
            # Re-running the installer is allowed, but silently replacing a different prior human
            # decision is not.  Exact equality is the clean idempotent case.
            old = by_family[family]
            if old != row:
                raise SystemExit(
                    f"workspace already contains a different decision for {family}; review the conflict manually before replacing it"
                )
        else:
            current.append(dict(row))
            by_family[family] = current[-1]

    workspace["schema_version"] = max(int(workspace.get("schema_version", 1) or 1), 1)
    workspace["dataset"] = "vegeta-s4"
    workspace["purpose"] = (
        "S4 cumulative human-review workspace. Reviewed rows are executable only in a candidate draft; "
        "pending rows are explicit review scaffolding and are ignored by the family-map applier."
    )
    workspace["decisions"] = sorted(current, key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", ""))))
    workspace["installed_second_batch_source"] = str(ns.second_batch)
    workspace["installed_second_batch_reviewed_families"] = reviewed
    workspace["installed_second_batch_pending_families"] = [str(r.get("runtime_code_family")) for r in pending]
    atomic(ns.workspace_decisions, workspace)

    pending_doc = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": "Pending second-batch S4 manual-review scaffold. These rows are not executable mappings.",
        "source": str(ns.second_batch),
        "pending_count": len(pending),
        "decisions": pending,
    }
    atomic(ns.pending_output, pending_doc)

    print(f"second-batch reviewed aliases installed: {len(reviewed)}")
    print(f"second-batch pending review rows retained: {len(pending)}")
    print(f"workspace decisions: {ns.workspace_decisions}")
    print(f"pending scaffold:   {ns.pending_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
