#!/usr/bin/env python3
"""Verify that Vegeta S1 reproduces the frozen S3 prefix before large native preparation."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Iterator


def rows(path: Path) -> Iterator[dict]:
    with path.open(encoding="utf-8") as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def tx_ids(block: dict) -> list[tuple[int, str]]:
    return [
        (int(tx.get("tx_index", i)), str(tx.get("tx_hash") or "").lower())
        for i, tx in enumerate(block.get("transactions") or [])
    ]


def plan_signature(block: dict) -> list[tuple]:
    out = []
    for tx in block.get("transactions") or []:
        actions = []
        for a in tx.get("native_actions") or []:
            actions.append((
                a.get("translation_status"), a.get("native_code_family"),
                a.get("native_instance_id"), a.get("native_entrypoint"),
                a.get("system_action_kind"), a.get("storage_context_address"),
                a.get("ethereum_code_address"), a.get("ethereum_msg_sender"),
            ))
        out.append((int(tx.get("tx_index", -1)), str(tx.get("tx_hash") or "").lower(), tuple(actions)))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--s1-corpus", type=Path, required=True)
    ap.add_argument("--s3-corpus", type=Path, required=True)
    ap.add_argument("--prefix-blocks", type=int, default=101)
    ap.add_argument("--s1-native-plan", type=Path)
    ap.add_argument("--s3-native-plan", type=Path)
    ap.add_argument("--strict-native-plan", action="store_true")
    ns = ap.parse_args()

    s1 = rows(ns.s1_corpus)
    s3 = rows(ns.s3_corpus)
    source_checked = 0
    for i in range(ns.prefix_blocks):
        try:
            a = next(s1); b = next(s3)
        except StopIteration as exc:
            raise SystemExit(f"prefix ended before {ns.prefix_blocks} blocks at index {i}") from exc
        if int(a["block_number"]) != int(b["block_number"]):
            raise SystemExit(f"source prefix block-number mismatch at index {i}: {a['block_number']} != {b['block_number']}")
        ah = str(a.get("block_hash") or "").lower(); bh = str(b.get("block_hash") or "").lower()
        if ah and bh and ah != bh:
            raise SystemExit(f"source prefix block-hash mismatch at block {a['block_number']}")
        if tx_ids(a) != tx_ids(b):
            raise SystemExit(f"source prefix transaction mismatch at block {a['block_number']}")
        source_checked += 1

    native_checked = native_mismatches = 0
    if ns.s1_native_plan and ns.s3_native_plan and ns.s1_native_plan.exists() and ns.s3_native_plan.exists():
        p1 = rows(ns.s1_native_plan); p3 = rows(ns.s3_native_plan)
        for i in range(ns.prefix_blocks):
            try:
                a = next(p1); b = next(p3)
            except StopIteration:
                break
            if int(a["block_number"]) != int(b["block_number"]) or tx_ids(a) != tx_ids(b):
                raise SystemExit(f"native-plan prefix identity mismatch at index {i}")
            native_checked += 1
            if plan_signature(a) != plan_signature(b):
                native_mismatches += 1
        if ns.strict_native_plan and native_mismatches:
            raise SystemExit(
                f"native-plan prefix drift: {native_mismatches}/{native_checked} S3-prefix blocks differ; "
                "review S1 resolver evidence or rerun without --strict-native-plan for diagnostics"
            )

    print("PASS: Vegeta S1 source prefix matches S3")
    print(f"source prefix blocks checked: {source_checked}")
    if native_checked:
        print(f"native plan prefix blocks checked: {native_checked}")
        print(f"native plan blocks with semantic drift: {native_mismatches}")
        if native_mismatches and not ns.strict_native_plan:
            print("NOTE: native-plan drift is diagnostic because larger-S1 proxy evidence may improve resolution; source identity still matches exactly.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
