#!/usr/bin/env python3
"""Freeze a human-reviewed S4 family map after dual semantic-surface gates pass."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


def read(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--draft-map", type=Path, required=True)
    ap.add_argument("--freeze-readiness", type=Path, required=True)
    ap.add_argument("--coverage", type=Path, required=True)
    ap.add_argument("--provenance", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--reviewed", action="store_true", help="explicit human attestation that all newly added mappings/selectors have been reviewed")
    ns = ap.parse_args()
    if not ns.reviewed:
        raise SystemExit("refusing to freeze without --reviewed human attestation")
    fmap = read(ns.draft_map)
    gate = read(ns.freeze_readiness)
    if fmap.get("dataset") != "vegeta-s4":
        raise SystemExit("draft map is not vegeta-s4")
    if not gate.get("ready_to_freeze_family_map"):
        raise SystemExit("family-review freeze gate is not PASS")
    for row in fmap.get("profile_mappings") or []:
        if not str(row.get("mapping_basis") or "").strip():
            raise SystemExit(f"mapping lacks mapping_basis: {row.get('ethereum_profile_family')}")
        if str(row.get("native_code_family") or "") not in (fmap.get("native_code_families") or {}):
            raise SystemExit(f"mapping references undeclared native family: {row.get('native_code_family')}")
    fmap["candidate_only"] = False
    fmap["status"] = "frozen-reviewed-s4-family-map-pending-native-plan-semantic-readiness"
    fmap["review_status"] = "HUMAN REVIEW ATTESTED; all-storage-access, structural conflict, median-block, and corpus-integrity family-freeze gates passed. Conflict-relevant access and gas metrics remain diagnostics; full selector/semantic/transaction/implementation gates are still required by prepare-native."
    fmap["freeze_evidence"] = {
        "draft_map_sha256": sha(ns.draft_map),
        "freeze_readiness_sha256": sha(ns.freeze_readiness),
        "coverage_sha256": sha(ns.coverage),
        "provenance_sha256": sha(ns.provenance),
    }
    atomic(ns.output, fmap)
    print(f"PASS: froze reviewed S4 family map -> {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
