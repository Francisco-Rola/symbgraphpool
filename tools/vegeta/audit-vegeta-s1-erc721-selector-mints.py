#!/usr/bin/env python3
"""Audit one opaque S1 ERC-721 selector against public zero-address Transfer logs.

This is a source-effect verification tool for selector review, not a native execution oracle. It
correlates an owner-scoped selector already present in native-plan.jsonl with committed ERC-721
Transfer(from=0) logs over the S1 block window.  The audit answers the questions needed before an
opaque selector can safely become an executable native mint adapter:

* how many selector calls commit vs execute inside reverted source scopes,
* how many ERC-721 mint events each committed call/transaction produces,
* whether reverted selector transactions produce no committed mint event,
* whether the mint recipient equals the EVM-visible msg.sender, and
* whether observed token IDs are sequential and fit the native u64 domain.

The report intentionally does not change semantic coverage.  It may be generated from roughly the
same ~20 chunked eth_getLogs calls used by the existing cw721-drop mint audit.
"""
from __future__ import annotations

import argparse
import json
import os
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

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


def int_hex(value: Any) -> int:
    if isinstance(value, int):
        return value
    text = str(value or "0").lower()
    return int(text, 16) if text.startswith("0x") else int(text)


def topic_addr(value: Any) -> str | None:
    text = str(value or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 64:
        return None
    return norm_addr("0x" + text[-40:])


def source_revert_scope_action_id(action: dict[str, Any], by_id: dict[int, dict[str, Any]]) -> int | None:
    """Return the top-most failed ancestor, matching native execution preparation semantics."""
    path: list[dict[str, Any]] = []
    current: dict[str, Any] | None = action
    seen: set[int] = set()
    while current is not None:
        aid = current.get("action_id")
        if aid is not None:
            aid_i = int(aid)
            if aid_i in seen:
                raise ValueError(f"cycle in native action parent chain at action {aid_i}")
            seen.add(aid_i)
        path.append(current)
        parent = current.get("parent_action_id")
        if parent is None:
            break
        current = by_id.get(int(parent))
        if current is None:
            break
    for node in reversed(path):
        if bool(node.get("failed_frame")):
            aid = node.get("action_id")
            return int(aid) if aid is not None else -1
    return None


def iter_plan_blocks(path: Path) -> Iterable[dict[str, Any]]:
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                yield json.loads(line)


def collect_selector_evidence(native_plan: Path, owner: str, selector: str) -> dict[str, dict[str, Any]]:
    txs: dict[str, dict[str, Any]] = {}
    for block in iter_plan_blocks(native_plan):
        block_number = int(block.get("block_number", -1))
        for tx in block.get("transactions") or []:
            actions = tx.get("native_actions") or []
            by_id = {
                int(action["action_id"]): action
                for action in actions
                if action.get("action_id") is not None
            }
            matches = [
                action for action in actions
                if norm_addr(action.get("storage_context_address")) == owner
                and str(action.get("selector") or "0x").lower() == selector
            ]
            if not matches:
                continue
            tx_hash = str(tx.get("tx_hash") or "").lower()
            if not tx_hash:
                raise ValueError(f"block {block_number}: selector transaction has no tx_hash")
            row = txs.setdefault(tx_hash, {
                "block_number": block_number,
                "tx_hash": tx_hash,
                "source_failed": bool(tx.get("source_failed")),
                "selector_actions": [],
            })
            for action in matches:
                scope = source_revert_scope_action_id(action, by_id)
                msg_sender = norm_addr(action.get("ethereum_msg_sender"))
                row["selector_actions"].append({
                    "action_id": action.get("action_id"),
                    "call_type": str(action.get("call_type") or "").upper(),
                    "code_address": norm_addr(action.get("ethereum_code_address")),
                    "msg_sender": msg_sender,
                    "failed_frame": bool(action.get("failed_frame")),
                    "source_revert_scope_action_id": scope,
                    "source_committed": not bool(tx.get("source_failed")) and scope is None,
                })
    return txs


def fetch_logs(client: RpcClient, owner: str, start: int, end: int, chunk: int) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    current = start
    while current <= end:
        hi = min(current + chunk - 1, end)
        stack = [(current, hi)]
        while stack:
            lo, upper = stack.pop()
            flt = {
                "fromBlock": hex(lo),
                "toBlock": hex(upper),
                "address": owner,
                "topics": [TRANSFER_TOPIC, ZERO_ADDRESS_TOPIC],
            }
            try:
                rows = client.call("eth_getLogs", [flt]) or []
            except RuntimeError:
                if lo >= upper:
                    raise
                mid = (lo + upper) // 2
                stack.append((mid + 1, upper))
                stack.append((lo, mid))
                continue
            if not isinstance(rows, list):
                raise RuntimeError(f"eth_getLogs returned non-list for {lo}..{upper}: {type(rows).__name__}")
            out.extend(row for row in rows if isinstance(row, dict))
        print(f"selector mint audit blocks={current}..{hi} events={len(out)}", flush=True)
        current = hi + 1
    return out


def parse_mint_logs(raw_logs: Iterable[dict[str, Any]], owner: str) -> tuple[dict[str, list[dict[str, Any]]], int]:
    by_tx: dict[str, list[dict[str, Any]]] = defaultdict(list)
    malformed = 0
    for row in raw_logs:
        if norm_addr(row.get("address")) != owner:
            continue
        topics = row.get("topics") or []
        if len(topics) < 4 or str(topics[0]).lower() != TRANSFER_TOPIC or str(topics[1]).lower() != ZERO_ADDRESS_TOPIC:
            malformed += 1
            continue
        recipient = topic_addr(topics[2])
        tx_hash = str(row.get("transactionHash") or "").lower()
        try:
            token_id = int_hex(topics[3])
            block_number = int_hex(row.get("blockNumber"))
            transaction_index = int_hex(row.get("transactionIndex"))
            log_index = int_hex(row.get("logIndex"))
        except (TypeError, ValueError):
            malformed += 1
            continue
        if not recipient or not tx_hash:
            malformed += 1
            continue
        by_tx[tx_hash].append({
            "recipient": recipient,
            "token_id": token_id,
            "block_number": block_number,
            "transaction_index": transaction_index,
            "log_index": log_index,
        })
    for events in by_tx.values():
        events.sort(key=lambda e: (e["block_number"], e["transaction_index"], e["log_index"]))
    return dict(by_tx), malformed


def analyze(owner: str, selector: str, evidence: dict[str, dict[str, Any]], logs_by_tx: dict[str, list[dict[str, Any]]], malformed: int = 0) -> dict[str, Any]:
    committed_action_count = 0
    reverted_action_count = 0
    committed_txs = 0
    reverted_only_txs = 0
    committed_zero = committed_one = committed_multi = 0
    reverted_with_mints = 0
    target_mint_events = 0
    recipient_checks = 0
    recipient_matches = 0
    cardinalities: Counter[int] = Counter()
    target_token_rows: list[dict[str, Any]] = []
    tx_rows: dict[str, dict[str, Any]] = {}

    for tx_hash, source in sorted(evidence.items(), key=lambda kv: (kv[1]["block_number"], kv[0])):
        actions = source.get("selector_actions") or []
        committed = [a for a in actions if a.get("source_committed")]
        reverted = [a for a in actions if not a.get("source_committed")]
        committed_action_count += len(committed)
        reverted_action_count += len(reverted)
        events = logs_by_tx.get(tx_hash, [])
        target_mint_events += len(events)
        cardinalities[len(events)] += 1
        if committed:
            committed_txs += 1
            if not events:
                committed_zero += 1
            elif len(events) == 1:
                committed_one += 1
            else:
                committed_multi += 1
        else:
            reverted_only_txs += 1
            if events:
                reverted_with_mints += 1

        senders = sorted({a["msg_sender"] for a in committed if a.get("msg_sender")})
        if committed and events and len(senders) == 1:
            for event in events:
                recipient_checks += 1
                if event["recipient"] == senders[0]:
                    recipient_matches += 1
        for event in events:
            target_token_rows.append(event)
        tx_rows[tx_hash] = {
            **source,
            "committed_selector_actions": len(committed),
            "reverted_selector_actions": len(reverted),
            "mint_events": events,
            "mint_event_count": len(events),
            "committed_msg_senders": senders,
        }

    target_token_rows.sort(key=lambda e: (e["block_number"], e["transaction_index"], e["log_index"]))
    target_token_ids = [int(row["token_id"]) for row in target_token_rows]
    target_sequential = all(right == left + 1 for left, right in zip(target_token_ids, target_token_ids[1:]))
    target_u64 = all(0 <= token_id <= U64_MAX for token_id in target_token_ids)
    target_distinct = len(set(target_token_ids))
    target_duplicate_events = len(target_token_ids) - target_distinct

    all_owner_rows = [event for events in logs_by_tx.values() for event in events]
    all_owner_rows.sort(key=lambda e: (e["block_number"], e["transaction_index"], e["log_index"]))
    owner_ids = [int(row["token_id"]) for row in all_owner_rows]
    owner_sequential = all(right == left + 1 for left, right in zip(owner_ids, owner_ids[1:]))
    target_hashes = set(evidence)
    extra_mint_txs = sorted(set(logs_by_tx) - target_hashes)

    all_committed_have_mints = committed_txs > 0 and committed_zero == 0
    exactly_one_per_committed_tx = committed_txs > 0 and committed_one == committed_txs
    no_reverted_mints = reverted_with_mints == 0
    recipient_is_sender = recipient_checks > 0 and recipient_matches == recipient_checks
    event_backed_adapter_ready = (
        all_committed_have_mints
        and exactly_one_per_committed_tx
        and no_reverted_mints
        and recipient_is_sender
        and target_u64
        and target_duplicate_events == 0
        and not extra_mint_txs
    )

    return {
        "schema_version": 1,
        "dataset": "vegeta-s1",
        "owner": owner,
        "selector": selector,
        "definition": "owner-scoped selector calls correlated with ERC721 Transfer(from=0) public logs",
        "summary": {
            "selector_transactions": len(evidence),
            "selector_actions": committed_action_count + reverted_action_count,
            "source_committed_selector_actions": committed_action_count,
            "source_reverted_selector_actions": reverted_action_count,
            "committed_selector_transactions": committed_txs,
            "reverted_only_selector_transactions": reverted_only_txs,
            "target_mint_events": target_mint_events,
            "committed_tx_with_zero_mint_events": committed_zero,
            "committed_tx_with_one_mint_event": committed_one,
            "committed_tx_with_multiple_mint_events": committed_multi,
            "reverted_tx_with_committed_mint_events": reverted_with_mints,
            "mint_event_count_distribution_by_selector_tx": {str(k): v for k, v in sorted(cardinalities.items())},
            "all_committed_selector_txs_have_mint_events": all_committed_have_mints,
            "exactly_one_mint_event_per_committed_selector_tx": exactly_one_per_committed_tx,
            "reverted_selector_txs_have_no_committed_mint_events": no_reverted_mints,
            "recipient_checks_against_single_committed_msg_sender": recipient_checks,
            "recipient_matches_msg_sender": recipient_matches,
            "all_checked_mint_recipients_equal_msg_sender": recipient_is_sender,
            "target_token_ids_sequential_plus_one": target_sequential,
            "target_token_ids_fit_u64": target_u64,
            "target_token_id_count": len(target_token_ids),
            "target_distinct_token_id_count": target_distinct,
            "target_duplicate_token_id_events": target_duplicate_events,
            "target_token_ids_unique": target_duplicate_events == 0,
            "first_target_token_id": target_token_ids[0] if target_token_ids else None,
            "last_target_token_id": target_token_ids[-1] if target_token_ids else None,
            "min_target_token_id": min(target_token_ids) if target_token_ids else None,
            "max_target_token_id": max(target_token_ids) if target_token_ids else None,
            "all_owner_mint_events": len(all_owner_rows),
            "all_owner_mint_transactions": len(logs_by_tx),
            "all_owner_token_ids_sequential_plus_one": owner_sequential,
            "extra_owner_mint_transactions_not_using_target_selector": len(extra_mint_txs),
            "event_backed_native_mint_adapter_ready": event_backed_adapter_ready,
            "ignored_malformed_logs": malformed,
            "concrete_storage_keys_used": False,
        },
        "extra_owner_mint_tx_hashes_sample": extra_mint_txs[:20],
        "transactions": tx_rows,
    }


def write_text_report(path: Path, report: dict[str, Any]) -> None:
    s = report["summary"]
    lines = [
        "Vegeta S1 owner-scoped ERC721 selector mint audit",
        "",
        f"owner: {report['owner']}",
        f"selector: {report['selector']}",
        f"selector transactions/actions: {s['selector_transactions']}/{s['selector_actions']}",
        f"source committed/reverted selector actions: {s['source_committed_selector_actions']}/{s['source_reverted_selector_actions']}",
        f"committed/reverted-only selector tx: {s['committed_selector_transactions']}/{s['reverted_only_selector_transactions']}",
        f"target ERC721 mint events: {s['target_mint_events']}",
        f"committed tx with 0/1/>1 mint events: {s['committed_tx_with_zero_mint_events']}/{s['committed_tx_with_one_mint_event']}/{s['committed_tx_with_multiple_mint_events']}",
        f"reverted selector tx with committed mint events: {s['reverted_tx_with_committed_mint_events']}",
        f"mint-event cardinality distribution: {s['mint_event_count_distribution_by_selector_tx']}",
        f"all committed selector tx have mint events: {s['all_committed_selector_txs_have_mint_events']}",
        f"exactly one mint event per committed selector tx: {s['exactly_one_mint_event_per_committed_selector_tx']}",
        f"reverted selector tx have no committed mint events: {s['reverted_selector_txs_have_no_committed_mint_events']}",
        f"mint recipient == EVM msg.sender: {s['all_checked_mint_recipients_equal_msg_sender']} ({s['recipient_matches_msg_sender']}/{s['recipient_checks_against_single_committed_msg_sender']})",
        f"target token IDs sequential +1: {s['target_token_ids_sequential_plus_one']}",
        f"target token IDs fit u64: {s['target_token_ids_fit_u64']}",
        f"target token IDs unique: {s['target_token_ids_unique']} (distinct={s['target_distinct_token_id_count']}/{s['target_token_id_count']})",
        f"target token IDs first/last: {s['first_target_token_id']}..{s['last_target_token_id']}",
        f"target token ID min/max: {s['min_target_token_id']}..{s['max_target_token_id']}",
        f"all owner mint events/transactions: {s['all_owner_mint_events']}/{s['all_owner_mint_transactions']}",
        f"all owner token IDs sequential +1: {s['all_owner_token_ids_sequential_plus_one']}",
        f"extra owner mint tx not using target selector: {s['extra_owner_mint_transactions_not_using_target_selector']}",
        f"event-backed native mint adapter ready: {s['event_backed_native_mint_adapter_ready']}",
        "",
        "Interpretation:",
        "  High conflict gain alone is never sufficient for selector promotion.",
        "  If the event-backed readiness check passes, an owner-scoped adapter may use the frozen public",
        "  Transfer log token ID/recipient for committed calls and a discarded synthetic key for reverts.",
    ]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--native-plan", type=Path, required=True)
    ap.add_argument("--owner", default="0x885523263378d6f27a5b8c533ad3b05ab9e105b5")
    ap.add_argument("--selector", default="0xfd883998")
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--logs-json", type=Path, help="offline raw eth_getLogs array for tests/review")
    ap.add_argument("--start-block", type=int, default=16_774_645)
    ap.add_argument("--end-block", type=int, default=16_779_644)
    ap.add_argument("--chunk-blocks", type=int, default=int(os.environ.get("VEGETA_S1_MINT_LOG_CHUNK_BLOCKS", "250")))
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ns = ap.parse_args()

    owner = norm_addr(ns.owner)
    selector = str(ns.selector).lower()
    if not owner:
        raise SystemExit(f"invalid --owner: {ns.owner}")
    if not (selector.startswith("0x") and len(selector) == 10):
        raise SystemExit(f"invalid four-byte --selector: {ns.selector}")
    if ns.end_block < ns.start_block or ns.chunk_blocks <= 0:
        raise SystemExit("invalid block range/chunk size")

    evidence = collect_selector_evidence(ns.native_plan, owner, selector)
    if not evidence:
        raise SystemExit(f"native plan contains no {owner} {selector} actions; regenerate current S1 native plan first")

    if ns.logs_json:
        raw_logs = json.loads(ns.logs_json.read_text())
        if not isinstance(raw_logs, list):
            raise SystemExit("--logs-json must contain a raw eth_getLogs JSON array")
    else:
        if not ns.rpc_url:
            raise SystemExit("ETH_RPC_URL/--rpc-url is required unless --logs-json is provided")
        client = RpcClient(ns.rpc_url, timeout=60, retries=5, backoff=1.0)
        raw_logs = fetch_logs(client, owner, ns.start_block, ns.end_block, ns.chunk_blocks)

    logs_by_tx, malformed = parse_mint_logs(raw_logs, owner)
    report = analyze(owner, selector, evidence, logs_by_tx, malformed)
    report["block_range"] = [ns.start_block, ns.end_block]

    ns.output.parent.mkdir(parents=True, exist_ok=True)
    tmp = ns.output.with_suffix(ns.output.suffix + ".tmp")
    tmp.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    tmp.replace(ns.output)
    write_text_report(ns.text_output, report)
    print(ns.text_output.read_text(), end="")
    print(f"JSON: {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
