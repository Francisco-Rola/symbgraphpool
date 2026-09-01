#!/usr/bin/env python3
"""Collect resumable Geth callTracer blocks from a Vegeta corpus without loading the corpus."""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from characterize_vegeta_corpus_compat import (  # type: ignore
    RpcClient, count_call_frames, iter_call_frames, normalize_address,
    normalize_block_call_trace_item,
)
from vegeta_corpus import iter_blocks


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def cache_valid(path: Path, block: dict) -> bool:
    if not path.exists():
        return False
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return False
    if int(cached.get("block_number", -1)) != int(block["block_number"]):
        return False
    if str(cached.get("block_hash") or "").lower() != str(block.get("block_hash") or "").lower():
        return False
    expected = block.get("transactions") or []
    observed = cached.get("transactions") or []
    if len(expected) != len(observed):
        return False
    return all(
        str(a.get("tx_hash") or "").lower() == str(b.get("tx_hash") or "").lower()
        for a, b in zip(expected, observed)
    )


def merge_addresses(path: Path, seen: dict[str, int], source: str) -> None:
    existing = {"schema_version": 1, "addresses": []}
    if path.exists():
        existing = json.loads(path.read_text(encoding="utf-8"))
    merged = {
        str(item["address"]).lower(): int(item["first_seen_block"])
        for item in existing.get("addresses") or []
    }
    for address, block in seen.items():
        merged[address] = min(block, merged.get(address, block))
    atomic_json(path, {
        "schema_version": 1,
        "source_corpus": existing.get("source_corpus") or source,
        "addresses": [
            {"address": address, "first_seen_block": merged[address]}
            for address in sorted(merged)
        ],
    })


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--cache-dir", type=Path, required=True)
    ap.add_argument("--relevant-addresses", type=Path, required=True)
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--rpc-timeout", type=int, default=600)
    ap.add_argument("--rpc-retries", type=int, default=5)
    ap.add_argument("--rpc-backoff", type=float, default=1.0)
    ap.add_argument("--trace-timeout", type=int, default=600)
    ap.add_argument("--reexec", type=int, default=128)
    ap.add_argument("--delay-ms", type=int, default=50)
    ap.add_argument("--start-block", type=int)
    ap.add_argument("--end-block", type=int)
    ns = ap.parse_args()
    if not ns.rpc_url:
        raise SystemExit("set ETH_RPC_URL or pass --rpc-url")
    client = RpcClient(ns.rpc_url, timeout=ns.rpc_timeout, retries=ns.rpc_retries, backoff=ns.rpc_backoff)
    ns.cache_dir.mkdir(parents=True, exist_ok=True)
    observed_addresses: dict[str, int] = {}
    processed = reused = traced = 0
    for block in iter_blocks(ns.corpus):
        bn = int(block["block_number"])
        if ns.start_block is not None and bn < ns.start_block:
            continue
        if ns.end_block is not None and bn > ns.end_block:
            continue
        processed += 1
        path = ns.cache_dir / f"{bn}.json"
        if cache_valid(path, block):
            reused += 1
            cached = json.loads(path.read_text(encoding="utf-8"))
        else:
            config = {
                "tracer": "callTracer",
                "tracerConfig": {"onlyTopCall": False, "withLog": False},
                "timeout": f"{ns.trace_timeout}s",
                "reexec": ns.reexec,
            }
            raw = client.call("debug_traceBlockByNumber", [hex(bn), config])
            txs = block.get("transactions") or []
            if not isinstance(raw, list) or len(raw) != len(txs):
                raise RuntimeError(f"block {bn}: expected {len(txs)} call traces, got {type(raw).__name__}/{len(raw) if isinstance(raw,list) else '-'}")
            normalized = []
            frames = 0
            for idx, (tx, item) in enumerate(zip(txs, raw)):
                row = normalize_block_call_trace_item(item, str(tx.get("tx_hash") or "").lower(), bn, idx)
                frames += count_call_frames(row["result"])
                normalized.append(row)
            cached = {
                "schema_version": 1,
                "trace_semantics": "geth-callTracer-v1",
                "block_number": bn,
                "block_hash": str(block.get("block_hash") or "").lower(),
                "transactions": normalized,
            }
            atomic_json(path, cached)
            traced += 1
            print(f"callTracer block={bn} tx={len(txs)} frames={frames} traced={traced} reused={reused}", flush=True)
            if ns.delay_ms:
                time.sleep(ns.delay_ms / 1000.0)
        for tx in cached.get("transactions") or []:
            root = tx.get("result") or {}
            for frame, _, _ in iter_call_frames(root):
                address = normalize_address(frame.get("to"))
                if address:
                    observed_addresses[address] = min(bn, observed_addresses.get(address, bn))
        if processed % 100 == 0:
            merge_addresses(ns.relevant_addresses, observed_addresses, str(ns.corpus))
            observed_addresses.clear()
    merge_addresses(ns.relevant_addresses, observed_addresses, str(ns.corpus))
    print(f"callTracer complete blocks={processed} traced={traced} reused={reused} cache={ns.cache_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
