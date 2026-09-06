#!/usr/bin/env python3
"""Characterize frozen Vegeta S4 corpus/callTracer/code caches without any RPC calls.

This is the first post-collection S4 step. It ranks direct destinations, storage owners,
runtime bytecode families, selectors, and call/delegatecall activity while preserving the
important boundary that concrete source storage accesses are used only for offline coverage
characterization and are never embedded into the native execution plan.
"""
from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from native_s3_planner_compat import normalize_address, runtime_code_family
from vegeta_corpus import iter_blocks, storage_contract

DELEGATE_TYPES = {"DELEGATECALL", "CALLCODE"}


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def walk(frame: dict[str, Any]):
    yield frame
    for child in frame.get("calls") or []:
        if isinstance(child, dict):
            yield from walk(child)


def code_family(code_cache: dict[str, dict], address: str | None) -> str | None:
    address = normalize_address(address)
    if not address:
        return None
    row = code_cache.get(address)
    return runtime_code_family(row.get("code")) if row else None


def selector_from_input(value: Any) -> str:
    text = str(value or "0x").lower()
    return text[:10] if text.startswith("0x") and len(text) >= 10 else "0x"


def pair_count(readers: set[int], writers: set[int]) -> int:
    touched = sorted(readers | writers)
    return sum(
        1
        for pos, left in enumerate(touched)
        for right in touched[pos + 1 :]
        if left in writers or right in writers
    )


def ranked(counter: Counter[str], top: int | None = None) -> list[dict[str, Any]]:
    rows = [{"key": key, "count": count} for key, count in counter.most_common(top)]
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--thin-corpus", type=Path, required=True)
    ap.add_argument("--call-cache", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--relevant-addresses", type=Path, required=True)
    ap.add_argument("--output-dir", type=Path, required=True)
    ap.add_argument("--top", type=int, default=100)
    ns = ap.parse_args()
    if ns.top <= 0:
        raise SystemExit("--top must be positive")

    for path in (ns.corpus, ns.thin_corpus, ns.code_cache, ns.relevant_addresses):
        if not path.exists():
            raise SystemExit(f"missing frozen S4 input: {path}")
    if not ns.call_cache.is_dir():
        raise SystemExit(f"missing frozen S4 callTracer cache: {ns.call_cache}")

    raw_cache = read_json(ns.code_cache)
    if not isinstance(raw_cache, dict):
        raise SystemExit(f"code cache must be a JSON object: {ns.code_cache}")
    code_cache = {str(k).lower(): v for k, v in raw_cache.items() if isinstance(v, dict)}

    relevant_doc = read_json(ns.relevant_addresses)
    relevant = {
        str(row.get("address") or "").lower()
        for row in relevant_doc.get("addresses") or []
        if isinstance(row, dict) and row.get("address")
    }
    missing_code = sorted(address for address in relevant if address not in code_cache)
    if missing_code:
        sample = ", ".join(missing_code[:5])
        raise SystemExit(
            f"historical code cache is incomplete: {len(missing_code)} relevant addresses missing; first: {sample}"
        )

    direct_tx: Counter[str] = Counter()
    direct_gas: Counter[str] = Counter()
    direct_selectors: Counter[str] = Counter()
    direct_family_tx: Counter[str] = Counter()
    direct_family_gas: Counter[str] = Counter()

    owner_access: Counter[str] = Counter()
    owner_tx: Counter[str] = Counter()
    owner_pair_attr: Counter[str] = Counter()
    owner_family_access: Counter[str] = Counter()
    owner_family_pair_attr: Counter[str] = Counter()

    frame_calls: Counter[str] = Counter()
    frame_selectors: Counter[str] = Counter()
    frame_family_calls: Counter[str] = Counter()
    delegate_targets: Counter[str] = Counter()
    delegate_family_calls: Counter[str] = Counter()

    blocks = txs = frames = delegate_frames = 0
    thin_iter = iter_blocks(ns.thin_corpus)

    for block in iter_blocks(ns.corpus):
        try:
            thin = next(thin_iter)
        except StopIteration as exc:
            raise SystemExit("thin corpus ended before source corpus") from exc
        bn = int(block["block_number"])
        if int(thin.get("block_number", -1)) != bn:
            raise SystemExit(f"block mismatch source={bn} thin={thin.get('block_number')}")
        source_txs = block.get("transactions") or []
        thin_txs = thin.get("transactions") or []
        if len(source_txs) != len(thin_txs):
            raise SystemExit(f"block {bn}: source tx={len(source_txs)} thin tx={len(thin_txs)}")

        call_path = ns.call_cache / f"{bn}.json"
        if not call_path.exists():
            raise SystemExit(f"missing callTracer cache for block {bn}: {call_path}")
        calls = read_json(call_path)
        call_txs = calls.get("transactions") or []
        if len(call_txs) != len(source_txs):
            raise SystemExit(f"block {bn}: source tx={len(source_txs)} callTracer tx={len(call_txs)}")
        expected_hash = str(block.get("block_hash") or "").lower()
        observed_hash = str(calls.get("block_hash") or "").lower()
        if expected_hash and observed_hash and expected_hash != observed_hash:
            raise SystemExit(f"block {bn}: callTracer block hash mismatch")

        key_readers: dict[str, set[int]] = defaultdict(set)
        key_writers: dict[str, set[int]] = defaultdict(set)
        owners_by_tx: list[set[str]] = []

        for idx, tx in enumerate(source_txs):
            txs += 1
            to = normalize_address(tx.get("to"))
            gas = int(tx.get("gas_used", 0) or 0)
            selector = str(tx.get("selector") or selector_from_input(tx.get("input"))).lower()
            if to:
                direct_tx[to] += 1
                direct_gas[to] += gas
                direct_selectors[f"{to}:{selector}"] += 1
                fam = code_family(code_cache, to)
                if fam:
                    direct_family_tx[fam] += 1
                    direct_family_gas[fam] += gas

            touched_owners: set[str] = set()
            for key in tx.get("reads") or []:
                key = str(key)
                key_readers[key].add(idx)
                owner = storage_contract(key)
                if owner:
                    address = "0x" + owner
                    touched_owners.add(address)
                    owner_access[address] += 1
            for key in tx.get("writes") or []:
                key = str(key)
                key_writers[key].add(idx)
                owner = storage_contract(key)
                if owner:
                    address = "0x" + owner
                    touched_owners.add(address)
                    owner_access[address] += 1
            owners_by_tx.append(touched_owners)

        for owners in owners_by_tx:
            for owner in owners:
                owner_tx[owner] += 1

        owner_pair_sets: dict[str, set[tuple[int, int]]] = defaultdict(set)
        for key in set(key_readers) | set(key_writers):
            writers = key_writers.get(key, set())
            if not writers:
                continue
            touched = sorted(key_readers.get(key, set()) | writers)
            pairs = {
                (left, right)
                for pos, left in enumerate(touched)
                for right in touched[pos + 1 :]
                if left in writers or right in writers
            }
            owner = storage_contract(key)
            if owner and pairs:
                owner_pair_sets["0x" + owner].update(pairs)
        for owner, pairs in owner_pair_sets.items():
            owner_pair_attr[owner] += len(pairs)

        for traced in call_txs:
            root = traced.get("result") or {}
            for frame in walk(root):
                frames += 1
                typ = str(frame.get("type") or "CALL").upper()
                address = normalize_address(frame.get("to"))
                selector = selector_from_input(frame.get("input"))
                if address:
                    frame_calls[address] += 1
                    frame_selectors[f"{address}:{selector}"] += 1
                    fam = code_family(code_cache, address)
                    if fam:
                        frame_family_calls[fam] += 1
                    if typ in DELEGATE_TYPES:
                        delegate_frames += 1
                        delegate_targets[address] += 1
                        if fam:
                            delegate_family_calls[fam] += 1
        blocks += 1
        if blocks % 250 == 0:
            print(f"S4 characterization blocks={blocks} tx={txs} frames={frames}", flush=True)

    try:
        extra = next(thin_iter)
    except StopIteration:
        extra = None
    if extra is not None:
        raise SystemExit(f"thin corpus contains extra block {extra.get('block_number')}")

    for owner, count in owner_access.items():
        fam = code_family(code_cache, owner)
        if fam:
            owner_family_access[fam] += count
    for owner, count in owner_pair_attr.items():
        fam = code_family(code_cache, owner)
        if fam:
            owner_family_pair_attr[fam] += count

    addresses: dict[str, dict[str, Any]] = {}
    for address in set(direct_tx) | set(owner_access) | set(frame_calls):
        addresses[address] = {
            "address": address,
            "runtime_code_family": code_family(code_cache, address),
            "direct_transactions": direct_tx[address],
            "direct_gas_used": direct_gas[address],
            "storage_transactions": owner_tx[address],
            "storage_access_records": owner_access[address],
            "conflict_owner_pair_attributions": owner_pair_attr[address],
            "call_frames": frame_calls[address],
            "delegate_target_frames": delegate_targets[address],
        }

    family_keys = set(direct_family_tx) | set(owner_family_access) | set(frame_family_calls)
    families = []
    for fam in family_keys:
        fam_addresses = [row for row in addresses.values() if row["runtime_code_family"] == fam]
        families.append({
            "runtime_code_family": fam,
            "addresses": len(fam_addresses),
            "direct_transactions": direct_family_tx[fam],
            "direct_gas_used": direct_family_gas[fam],
            "storage_access_records": owner_family_access[fam],
            "conflict_owner_pair_attributions": owner_family_pair_attr[fam],
            "call_frames": frame_family_calls[fam],
            "delegate_target_frames": delegate_family_calls[fam],
            "top_addresses": sorted(
                fam_addresses,
                key=lambda r: (-r["conflict_owner_pair_attributions"], -r["storage_access_records"], -r["direct_gas_used"], r["address"]),
            )[:20],
        })
    families.sort(key=lambda r: (-r["conflict_owner_pair_attributions"], -r["storage_access_records"], -r["direct_gas_used"], r["runtime_code_family"]))

    report = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "input_boundary": "frozen source corpus + thin corpus + callTracer cache + historical runtime-code cache; no RPC and no exact SLOAD/SSTORE traces",
        "blocks": blocks,
        "transactions": txs,
        "call_frames": frames,
        "delegate_frames": delegate_frames,
        "runtime_families": families,
        "top_addresses": sorted(
            addresses.values(),
            key=lambda r: (-r["conflict_owner_pair_attributions"], -r["storage_access_records"], -r["direct_gas_used"], r["address"]),
        )[: ns.top],
    }
    selectors = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "direct_destination_selectors": [
            {"address": key.split(":", 1)[0], "selector": key.split(":", 1)[1], "transactions": count}
            for key, count in direct_selectors.most_common(ns.top)
        ],
        "call_frame_selectors": [
            {"address": key.split(":", 1)[0], "selector": key.split(":", 1)[1], "frames": count}
            for key, count in frame_selectors.most_common(ns.top)
        ],
    }

    ns.output_dir.mkdir(parents=True, exist_ok=True)
    atomic_json(ns.output_dir / "family-summary.json", report)
    atomic_json(ns.output_dir / "selector-summary.json", selectors)
    lines = [
        "Vegeta S4 frozen family characterization",
        "",
        f"blocks: {blocks}",
        f"transactions: {txs}",
        f"callTracer frames: {frames} (delegate/callcode={delegate_frames})",
        f"runtime bytecode families observed: {len(families)}",
        "",
        "Top runtime families by source conflict-owner attribution:",
    ]
    for row in families[:20]:
        lines.append(
            f"  {row['runtime_code_family']} pairs={row['conflict_owner_pair_attributions']} "
            f"storage={row['storage_access_records']} direct_tx={row['direct_transactions']} "
            f"gas={row['direct_gas_used']} frames={row['call_frames']}"
        )
    lines.extend([
        "",
        "This report is characterization only. A runtime-code match can reuse previously reviewed semantics;",
        "new or unmapped families/selectors still require explicit semantic review before native preparation.",
    ])
    text = "\n".join(lines) + "\n"
    (ns.output_dir / "family-summary.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    print(f"wrote {ns.output_dir / 'family-summary.json'}")
    print(f"wrote {ns.output_dir / 'selector-summary.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
