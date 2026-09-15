#!/usr/bin/env python3
"""Collect and validate public ERC-721 mint events for reviewed sequential cw721-drop owners.

This is a narrow publication-fidelity input, not an access oracle. It reads only public ERC-721
Transfer logs with `from == address(0)`. With --native-plan, the owner set is restricted to instances
that actually use a reviewed sequential mint adapter and the block range is derived from that plan.
No concrete storage slots or SLOAD/SSTORE traces are queried or persisted.
"""
from __future__ import annotations

import argparse
import json
import os
from collections import defaultdict
from pathlib import Path
from typing import Any

from characterize_vegeta_corpus_compat import RpcClient

TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
ZERO_ADDRESS_TOPIC = "0x" + ("00" * 32)
U64_MAX = (1 << 64) - 1


def norm_addr(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 40:
        return None
    try:
        int(text, 16)
    except ValueError:
        return None
    return "0x" + text


def reviewed_drop_owners(family_map: dict) -> list[str]:
    owners: set[str] = set()
    for row in family_map.get("profile_mappings") or []:
        if row.get("native_code_family") != "cw721-drop":
            continue
        for address in row.get("storage_owner_scope") or []:
            normalized = norm_addr(address)
            if normalized:
                owners.add(normalized)
    return sorted(owners)




def sequential_drop_owners_from_plan(path: Path) -> tuple[list[str], int, int]:
    owners: set[str] = set()
    first: int | None = None
    last: int | None = None
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            block=json.loads(line); bn=int(block["block_number"])
            first=bn if first is None else min(first,bn); last=bn if last is None else max(last,bn)
            for tx in block.get("transactions", []):
                for action in tx.get("native_actions", []):
                    if action.get("native_code_family") != "cw721-drop":
                        continue
                    ep=str(action.get("native_entrypoint") or "").lower().replace("_", "")
                    if not ep.startswith("execute::") or not any(x in ep for x in ("mint","purchase","airdrop","reservedrop")):
                        continue
                    owner=norm_addr(action.get("storage_context_address") or action.get("ethereum_code_address"))
                    if owner: owners.add(owner)
    if first is None or last is None:
        raise SystemExit(f"native plan contains no blocks: {path}")
    return sorted(owners), first, last

def int_hex(value: Any) -> int:
    if isinstance(value, int):
        return value
    text = str(value or "0").lower()
    return int(text, 16) if text.startswith("0x") else int(text)


def fetch_logs(client: RpcClient, owners: list[str], start: int, end: int, chunk: int) -> list[dict]:
    out: list[dict] = []
    current = start
    while current <= end:
        hi = min(current + chunk - 1, end)
        stack = [(current, hi)]
        while stack:
            lo, upper = stack.pop()
            flt = {
                "fromBlock": hex(lo),
                "toBlock": hex(upper),
                "address": owners if len(owners) > 1 else owners[0],
                "topics": [TRANSFER_TOPIC, ZERO_ADDRESS_TOPIC],
            }
            try:
                rows = client.call("eth_getLogs", [flt]) or []
            except RuntimeError:
                if lo >= upper:
                    raise
                mid = (lo + upper) // 2
                # Preserve ascending output after stack LIFO processing.
                stack.append((mid + 1, upper))
                stack.append((lo, mid))
                continue
            if not isinstance(rows, list):
                raise RuntimeError(f"eth_getLogs returned non-list for {lo}..{upper}: {type(rows).__name__}")
            out.extend(row for row in rows if isinstance(row, dict))
        print(f"cw721-drop mint logs blocks={current}..{hi} events={len(out)}", flush=True)
        current = hi + 1
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--native-plan", type=Path, default=None, help="restrict to reviewed sequential-mint owners and derive block range")
    ap.add_argument("--dataset-label", default="vegeta-s1")
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--start-block", type=int, default=None)
    ap.add_argument("--end-block", type=int, default=None)
    ap.add_argument("--chunk-blocks", type=int, default=int(os.environ.get("VEGETA_MINT_LOG_CHUNK_BLOCKS", os.environ.get("VEGETA_S1_MINT_LOG_CHUNK_BLOCKS", "250"))))
    ap.add_argument("--output", type=Path, required=True)
    ns = ap.parse_args()
    if not ns.rpc_url:
        raise SystemExit("ETH_RPC_URL/--rpc-url is required to collect ERC721 mint logs")
    family_map = json.loads(ns.family_map.read_text())
    if ns.native_plan is not None:
        owners, plan_start, plan_end = sequential_drop_owners_from_plan(ns.native_plan)
        start = plan_start if ns.start_block is None else ns.start_block
        end = plan_end if ns.end_block is None else ns.end_block
    else:
        owners = reviewed_drop_owners(family_map)
        start = 16_774_645 if ns.start_block is None else ns.start_block
        end = 16_779_644 if ns.end_block is None else ns.end_block
    if end < start or ns.chunk_blocks <= 0:
        raise SystemExit("invalid block range/chunk size")
    if not owners:
        raise SystemExit("reviewed input contains no sequential cw721-drop storage owners")

    client = RpcClient(ns.rpc_url, timeout=60, retries=5, backoff=1.0)
    logs = fetch_logs(client, owners, start, end, ns.chunk_blocks)
    by_owner: dict[str, list[dict]] = defaultdict(list)
    ignored = 0
    for row in logs:
        owner = norm_addr(row.get("address"))
        topics = row.get("topics") or []
        if owner not in owners or len(topics) < 4:
            ignored += 1
            continue
        try:
            token_id = int_hex(topics[3])
            recipient_topic = str(topics[2] or "").lower()
            recipient = norm_addr("0x" + recipient_topic.replace("0x", "")[-40:])
        except (ValueError, TypeError):
            ignored += 1
            continue
        if recipient is None:
            ignored += 1
            continue
        tx_hash = str(row.get("transactionHash") or "").lower()
        by_owner[owner].append(
            {
                "block_number": int_hex(row.get("blockNumber")),
                "transaction_index": int_hex(row.get("transactionIndex")),
                "log_index": int_hex(row.get("logIndex")),
                "tx_hash": tx_hash,
                "token_id": token_id,
                "recipient": recipient,
            }
        )

    owner_reports: dict[str, dict] = {}
    all_sequential = True
    all_u64 = True
    for owner in owners:
        events = sorted(by_owner.get(owner, []), key=lambda x: (x["block_number"], x["transaction_index"], x["log_index"]))
        token_ids = [int(x["token_id"]) for x in events]
        sequential = all(right == left + 1 for left, right in zip(token_ids, token_ids[1:]))
        fits_u64 = all(0 <= token_id <= U64_MAX for token_id in token_ids)
        all_sequential = all_sequential and sequential
        all_u64 = all_u64 and fits_u64
        txs: dict[str, dict] = {}
        for event in events:
            tx_hash = event["tx_hash"]
            row = txs.setdefault(
                tx_hash,
                {
                    "block_number": event["block_number"],
                    "transaction_index": event["transaction_index"],
                    "mint_count": 0,
                    "token_ids": [],
                    "recipients": [],
                },
            )
            row["mint_count"] += 1
            row["token_ids"].append(event["token_id"])
            row["recipients"].append(event["recipient"])
        owner_reports[owner] = {
            "mint_events": len(events),
            "mint_transactions": len(txs),
            "first_token_id": token_ids[0] if token_ids else None,
            "last_token_id": token_ids[-1] if token_ids else None,
            "sequential_plus_one": sequential,
            "all_token_ids_fit_u64": fits_u64,
            "max_mints_per_transaction": max((int(v["mint_count"]) for v in txs.values()), default=0),
            "transactions": dict(sorted(txs.items())),
        }

    report = {
        "schema_version": 2,
        "dataset": ns.dataset_label,
        "definition": "ERC721 Transfer(address,address,uint256) logs with indexed from == address(0) for reviewed cw721-drop storage owners; includes indexed recipient for execution-effect reconciliation",
        "block_range": [start, end],
        "reviewed_owners": owners,
        "owners": owner_reports,
        "summary": {
            "owners": len(owners),
            "owners_with_mints": sum(bool(row["mint_events"]) for row in owner_reports.values()),
            "mint_events": sum(int(row["mint_events"]) for row in owner_reports.values()),
            "mint_transactions": sum(int(row["mint_transactions"]) for row in owner_reports.values()),
            "all_observed_sequences_plus_one": all_sequential,
            "all_token_ids_fit_u64": all_u64,
            "ignored_malformed_logs": ignored,
            "concrete_storage_keys_used": False,
        },
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    tmp = ns.output.with_suffix(ns.output.suffix + ".tmp")
    tmp.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    tmp.replace(ns.output)
    print(json.dumps(report["summary"], indent=2, sort_keys=True))
    print(f"wrote {ns.output}")
    if not all_u64:
        raise SystemExit("reviewed cw721-drop owner emitted token ID outside native u64 domain")
    if not all_sequential:
        raise SystemExit("reviewed cw721-drop owner has non-sequential in-window mint IDs; do not use state-derived MintDrop for that owner")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
