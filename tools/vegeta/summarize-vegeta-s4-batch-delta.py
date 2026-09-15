#!/usr/bin/env python3
"""Summarize exact before/after S4 family coverage for one reviewed batch."""
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


def metrics(doc: dict[str, Any]) -> dict[str, int | float]:
    conflict = doc.get("source_conflict_coverage") or {}
    storage = doc.get("storage_access_coverage") or {}
    total_conflict = int(conflict.get("total_unique_conflict_pairs") or 0)
    mapped_conflict = int(conflict.get("selected_family_unique_conflict_pairs") or 0)
    total_access = int(storage.get("total_access_records") or 0)
    mapped_access = int(storage.get("selected_family_access_records") or 0)
    return {
        "total_conflict_pairs": total_conflict,
        "mapped_conflict_pairs": mapped_conflict,
        "conflict_coverage": mapped_conflict / total_conflict if total_conflict else 1.0,
        "total_storage_accesses": total_access,
        "mapped_storage_accesses": mapped_access,
        "storage_access_coverage": mapped_access / total_access if total_access else 1.0,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--before", type=Path, required=True)
    ap.add_argument("--after", type=Path, required=True)
    ap.add_argument("--batch-label", default="review batch")
    ap.add_argument("--target-conflict", type=float, default=0.95)
    ap.add_argument("--target-storage-access", type=float, default=0.90)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ns = ap.parse_args()

    before = metrics(read(ns.before))
    after = metrics(read(ns.after))
    if before["total_conflict_pairs"] != after["total_conflict_pairs"]:
        raise SystemExit("before/after conflict denominators differ; refusing misleading batch delta")
    if before["total_storage_accesses"] != after["total_storage_accesses"]:
        raise SystemExit("before/after storage-access denominators differ; refusing misleading batch delta")

    conflict_target = math.ceil(ns.target_conflict * int(after["total_conflict_pairs"]))
    access_target = math.ceil(ns.target_storage_access * int(after["total_storage_accesses"]))
    out = {
        "schema_version": 2,
        "dataset": "vegeta-s4",
        "batch_label": ns.batch_label,
        "before": before,
        "after": after,
        "delta": {
            "newly_mapped_conflict_pairs": int(after["mapped_conflict_pairs"]) - int(before["mapped_conflict_pairs"]),
            "newly_mapped_storage_accesses": int(after["mapped_storage_accesses"]) - int(before["mapped_storage_accesses"]),
            "conflict_coverage_points": float(after["conflict_coverage"]) - float(before["conflict_coverage"]),
            "storage_access_coverage_points": float(after["storage_access_coverage"]) - float(before["storage_access_coverage"]),
        },
        "targets": {
            "conflict_fraction": ns.target_conflict,
            "conflict_pairs": conflict_target,
            "storage_access_fraction": ns.target_storage_access,
            "storage_access_records": access_target,
        },
        "remaining": {
            "conflict_pairs": max(0, conflict_target - int(after["mapped_conflict_pairs"])),
            "storage_access_records": max(0, access_target - int(after["mapped_storage_accesses"])),
        },
        "scheduler_fidelity_gates": {
            "conflict": int(after["mapped_conflict_pairs"]) >= conflict_target,
        },
        "diagnostic_references": {
            "storage_access": int(after["mapped_storage_accesses"]) >= access_target,
        },
        "publication_gates": {  # compatibility alias; storage_access is diagnostic in schema v2 policy
            "conflict": int(after["mapped_conflict_pairs"]) >= conflict_target,
            "storage_access": int(after["mapped_storage_accesses"]) >= access_target,
        },
    }
    atomic(ns.output, out)
    lines = [
        f"Vegeta S4 exact batch delta: {ns.batch_label}",
        "",
        f"conflict pairs: {before['mapped_conflict_pairs']}/{before['total_conflict_pairs']} ({100*float(before['conflict_coverage']):.2f}%) -> {after['mapped_conflict_pairs']}/{after['total_conflict_pairs']} ({100*float(after['conflict_coverage']):.2f}%)  delta=+{out['delta']['newly_mapped_conflict_pairs']}",
        f"all storage accesses: {before['mapped_storage_accesses']}/{before['total_storage_accesses']} ({100*float(before['storage_access_coverage']):.2f}%) -> {after['mapped_storage_accesses']}/{after['total_storage_accesses']} ({100*float(after['storage_access_coverage']):.2f}%)  delta=+{out['delta']['newly_mapped_storage_accesses']}",
        f"remaining conflict pairs to {100*ns.target_conflict:.2f}%: {out['remaining']['conflict_pairs']}",
        f"all-storage diagnostic shortfall to {100*ns.target_storage_access:.2f}% reference: {out['remaining']['storage_access_records']}",
        f"scheduler-fidelity conflict gate: {'PASS' if out['scheduler_fidelity_gates']['conflict'] else 'FAIL'}",
        f"storage-access diagnostic reference: {'REACHED' if out['diagnostic_references']['storage_access'] else 'OPEN'}",
    ]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
