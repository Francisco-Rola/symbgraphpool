#!/usr/bin/env python3
"""Install the checked-in S4 first-review batch into a generated review workspace.

This helper is intentionally narrow: it merges only checked-in S4 native-family extensions into the
persistent review base and writes the checked-in reviewed decisions to the workspace decision file.
The resulting family map is still candidate-only; exact coverage and the normal freeze gate remain
authoritative.
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
    ap.add_argument("--family-extension", type=Path, required=True)
    ap.add_argument("--reviewed-decisions", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ns = ap.parse_args()

    base = read(ns.review_base)
    ext = read(ns.family_extension)
    decisions = read(ns.reviewed_decisions)
    for label, doc in (("review base", base), ("family extension", ext), ("reviewed decisions", decisions)):
        if doc.get("dataset") != "vegeta-s4":
            raise SystemExit(f"{label} must have dataset=vegeta-s4")

    native = dict(base.get("native_code_families") or {})
    for name, config in sorted((ext.get("native_code_families") or {}).items()):
        if name in native and native[name] != config:
            raise SystemExit(f"refusing to replace a different existing native family definition: {name}")
        native[name] = config
    base["native_code_families"] = native
    base["expected_native_code_families"] = len(native)
    base["candidate_only"] = True
    base["s4_first_batch_native_extension"] = str(ns.family_extension)
    atomic(ns.review_base, base)

    rows = decisions.get("decisions") or []
    expected_priorities = {1, 2, 3, 4, 5, 6}
    observed = {int(row.get("priority", -1)) for row in rows}
    if observed != expected_priorities or any(str(row.get("review_status") or "") != "reviewed" for row in rows):
        raise SystemExit("checked-in first-batch decisions must contain exactly reviewed priorities 1..6")
    atomic(ns.workspace_decisions, decisions)

    print(f"installed {len(native)} native family definitions in {ns.review_base}")
    print(f"installed {len(rows)} reviewed first-batch decisions in {ns.workspace_decisions}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
