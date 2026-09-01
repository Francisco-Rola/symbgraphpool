#!/usr/bin/env python3
"""Fetch historical runtime bytecode for a frozen address/first-seen-block list, resumably.

Unlike the original S3 helper, this large-workload variant checkpoints the complete JSON cache in
batches instead of rewriting it after every address.  At S1 scale that avoids quadratic filesystem
work while still bounding restart loss to at most ``--checkpoint-every`` RPCs.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from characterize_vegeta_corpus_compat import RpcClient, normalize_runtime_code  # type: ignore


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def load_cache(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SystemExit(f"code cache must be a JSON object: {path}")
    return {str(k).lower(): v for k, v in value.items() if isinstance(v, dict)}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--addresses", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--rpc-timeout", type=int, default=120)
    ap.add_argument("--rpc-retries", type=int, default=5)
    ap.add_argument("--rpc-backoff", type=float, default=1.0)
    ap.add_argument("--delay-ms", type=int, default=20)
    ap.add_argument("--checkpoint-every", type=int, default=100)
    ns = ap.parse_args()
    if not ns.rpc_url:
        raise SystemExit("set ETH_RPC_URL or pass --rpc-url")
    raw = json.loads(ns.addresses.read_text(encoding="utf-8"))
    targets = {
        str(item["address"]).lower(): int(item["first_seen_block"])
        for item in raw.get("addresses") or []
    }
    cache = load_cache(ns.output)
    client = RpcClient(ns.rpc_url, timeout=ns.rpc_timeout, retries=ns.rpc_retries, backoff=ns.rpc_backoff)
    pending_since_checkpoint = 0
    fetched = reused = 0
    addresses = sorted(targets)
    for index, address in enumerate(addresses, start=1):
        target_block = targets[address]
        old = cache.get(address)
        if old and int(old.get("block_number", -1)) == target_block and "code" in old:
            reused += 1
            continue
        code = client.call("eth_getCode", [address, hex(target_block)])
        normalized = normalize_runtime_code(code)
        cache[address] = {"block_number": target_block, "code": "0x" + normalized}
        fetched += 1
        pending_since_checkpoint += 1
        if index == 1 or index % 100 == 0 or index == len(addresses):
            print(
                f"code [{index}/{len(addresses)}] fetched={fetched} reused={reused} "
                f"{address} @ {target_block} bytes={len(normalized)//2}", flush=True,
            )
        if pending_since_checkpoint >= max(ns.checkpoint_every, 1):
            atomic_json(ns.output, cache)
            pending_since_checkpoint = 0
        if ns.delay_ms > 0:
            time.sleep(ns.delay_ms / 1000.0)
    if pending_since_checkpoint or not ns.output.exists():
        atomic_json(ns.output, cache)
    print(f"wrote/reused {ns.output} addresses={len(targets)} fetched={fetched} reused={reused}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
