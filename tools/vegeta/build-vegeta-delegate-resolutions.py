#!/usr/bin/env python3
"""Derive conservative proxy/delegate profile resolutions from frozen callTracer + bytecode caches."""
from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path

from native_s3_planner_compat import normalize_address, runtime_code_family
from vegeta_corpus import iter_blocks

DELEGATE = {"DELEGATECALL", "CALLCODE"}
CALL = {"CALL", "STATICCALL"}


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def walk(frame: dict, storage_context: str | None = None):
    typ = str(frame.get("type") or "CALL").upper()
    code_address = normalize_address(frame.get("to"))
    if storage_context is None:
        storage_context = code_address
    yield frame, storage_context, code_address
    for child in frame.get("calls") or []:
        child_type = str(child.get("type") or "CALL").upper()
        child_to = normalize_address(child.get("to"))
        if child_type in DELEGATE:
            child_context = storage_context
        elif child_type in CALL:
            child_context = child_to
        else:
            child_context = child_to or storage_context
        yield from walk(child, child_context)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True, help="thin or full corpus; only block numbers are used")
    ap.add_argument("--call-cache", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ns = ap.parse_args()
    code_cache = {str(k).lower(): v for k, v in read_json(ns.code_cache).items() if isinstance(v, dict)}
    family_map = read_json(ns.family_map)
    profile_to_native = {
        str(row["ethereum_profile_family"]): str(row["native_code_family"])
        for row in family_map.get("profile_mappings") or []
    }
    evidence: dict[str, Counter[str]] = defaultdict(Counter)
    frames = delegate_frames = 0
    for block in iter_blocks(ns.corpus):
        bn = int(block["block_number"])
        path = ns.call_cache / f"{bn}.json"
        if not path.exists():
            raise SystemExit(f"missing callTracer cache block {bn}: {path}")
        cached = read_json(path)
        for tx in cached.get("transactions") or []:
            root = tx.get("result") or {}
            for frame, storage_context, code_address in walk(root):
                frames += 1
                typ = str(frame.get("type") or "").upper()
                if typ not in DELEGATE or not storage_context or not code_address:
                    continue
                delegate_frames += 1
                entry = code_cache.get(code_address)
                profile = runtime_code_family(entry.get("code")) if entry else None
                if profile in profile_to_native:
                    evidence[storage_context][profile] += 1
    records = []
    ambiguous = []
    for owner, counts in sorted(evidence.items()):
        native_counts: dict[str, int] = defaultdict(int)
        for profile, count in counts.items():
            native_counts[profile_to_native[profile]] += count
        if len(native_counts) != 1:
            ambiguous.append({
                "storage_owner": owner,
                "profile_counts": dict(counts),
                "native_family_counts": dict(native_counts),
                "reason": "delegate targets map to multiple native families",
            })
            continue
        chosen_profile, chosen_count = sorted(counts.items(), key=lambda x: (-x[1], x[0]))[0]
        records.append({
            "storage_owner": owner,
            "recommended_profile_family": chosen_profile,
            "recommended_native_code_family": profile_to_native[chosen_profile],
            "resolution_status": "calltrace-delegate-target-family",
            "mapped_delegate_frames": sum(counts.values()),
            "chosen_profile_frames": chosen_count,
            "profile_counts": dict(sorted(counts.items())),
        })
    out = {
        "schema_version": 1,
        "method": "streamed callTracer DELEGATECALL/CALLCODE target-family resolution; ambiguous multi-native-family owners remain unresolved",
        "total_call_frames": frames,
        "delegate_frames": delegate_frames,
        "resolution_records": records,
        "ambiguous_records": ambiguous,
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"delegate resolutions: resolved={len(records)} ambiguous={len(ambiguous)} delegate_frames={delegate_frames}")
    print(f"wrote {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
