#!/usr/bin/env python3
"""Characterize one owner-scoped opaque ERC-721 selector from public logs and calldata shape.

This is a review tool, not a semantic promotion.  It correlates selector actions already present in
S1 native-plan.jsonl with all public logs emitted by the storage owner over the S1 block window.
For ERC-721 Transfer events it classifies mint / burn / ordinary transfer effects; every other event
is retained by topic0 so a state-changing selector that has no Transfer effect is still visible.

The report also summarizes calldata byte lengths, msg.value, EVM-visible msg.sender, committed vs
reverted source scopes, and ABI-word shape.  It deliberately does not use historical concrete
storage keys and does not modify coverage.  Its purpose is to decide whether a selector has enough
target-specific evidence for an owner-scoped native adapter or needs source/ABI review first.
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
ZERO_ADDRESS = "0x" + "00" * 20


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


def int_value(value: Any) -> int:
    if isinstance(value, int):
        return value
    text = str(value or "0")
    return int(text, 16) if text.lower().startswith("0x") else int(text)


def topic_addr(value: Any) -> str | None:
    text = str(value or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 64:
        return None
    return norm_addr("0x" + text[-40:])


def source_revert_scope_action_id(action: dict[str, Any], by_id: dict[int, dict[str, Any]]) -> int | None:
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


def calldata_bytes(value: Any) -> bytes:
    text = str(value or "0x").lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) % 2:
        text = "0" + text
    try:
        return bytes.fromhex(text)
    except ValueError:
        return b""


def abi_word_shape(word: bytes, payload_len: int) -> str:
    if len(word) != 32:
        return "partial"
    value = int.from_bytes(word, "big")
    # ABI dynamic offsets are word-aligned and point after the static head.  We cannot know the
    # exact head length without the ABI, so this is intentionally only a shape hint.
    if value >= 32 and value % 32 == 0 and value < payload_len:
        return "possible-offset"
    if word[:12] == b"\x00" * 12 and any(word[12:]):
        return "address-shaped"
    if value <= (1 << 64) - 1:
        return "small-u64"
    if value == 0:
        return "zero"
    return "word256"


def collect_selector_evidence(native_plan: Path, owner: str, selector: str) -> dict[str, dict[str, Any]]:
    txs: dict[str, dict[str, Any]] = {}
    for block in iter_plan_blocks(native_plan):
        bn = int(block.get("block_number", -1))
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
                raise ValueError(f"block {bn}: selector transaction has no tx_hash")
            row = txs.setdefault(tx_hash, {
                "block_number": bn,
                "tx_hash": tx_hash,
                "source_failed": bool(tx.get("source_failed")),
                "selector_actions": [],
            })
            for action in matches:
                scope = source_revert_scope_action_id(action, by_id)
                raw = calldata_bytes(action.get("ethereum_input"))
                args = raw[4:] if len(raw) >= 4 else b""
                words = [args[i:i + 32] for i in range(0, len(args), 32)] if args else []
                row["selector_actions"].append({
                    "action_id": action.get("action_id"),
                    "call_type": str(action.get("call_type") or "").upper(),
                    "code_address": norm_addr(action.get("ethereum_code_address")),
                    "msg_sender": norm_addr(action.get("ethereum_msg_sender")),
                    "value": int_value(action.get("ethereum_value")),
                    "failed_frame": bool(action.get("failed_frame")),
                    "source_revert_scope_action_id": scope,
                    "source_committed": not bool(tx.get("source_failed")) and scope is None,
                    "calldata_bytes": len(raw),
                    "argument_bytes": len(args),
                    "abi_word_count_ceil": len(words),
                    "abi_word_shapes": [abi_word_shape(word, len(args)) for word in words[:8]],
                    "abi_word_values_hex": ["0x" + word.hex() for word in words[:8]],
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
            try:
                rows = client.call("eth_getLogs", [{
                    "fromBlock": hex(lo),
                    "toBlock": hex(upper),
                    "address": owner,
                }]) or []
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
        print(f"selector effect audit blocks={current}..{hi} logs={len(out)}", flush=True)
        current = hi + 1
    return out


def parse_owner_logs(raw_logs: Iterable[dict[str, Any]], owner: str) -> tuple[dict[str, list[dict[str, Any]]], int]:
    by_tx: dict[str, list[dict[str, Any]]] = defaultdict(list)
    malformed = 0
    for row in raw_logs:
        if norm_addr(row.get("address")) != owner:
            continue
        topics = [str(topic).lower() for topic in (row.get("topics") or [])]
        tx_hash = str(row.get("transactionHash") or "").lower()
        if not topics or not tx_hash:
            malformed += 1
            continue
        try:
            block_number = int_value(row.get("blockNumber"))
            transaction_index = int_value(row.get("transactionIndex"))
            log_index = int_value(row.get("logIndex"))
        except (TypeError, ValueError):
            malformed += 1
            continue
        parsed: dict[str, Any] = {
            "topic0": topics[0],
            "topics": topics,
            "data": str(row.get("data") or "0x").lower(),
            "block_number": block_number,
            "transaction_index": transaction_index,
            "log_index": log_index,
            "erc721_transfer_kind": None,
        }
        if topics[0] == TRANSFER_TOPIC and len(topics) >= 4:
            src = topic_addr(topics[1])
            dst = topic_addr(topics[2])
            try:
                token_id = int_value(topics[3])
            except (TypeError, ValueError):
                token_id = None
            if src and dst and token_id is not None:
                if src == ZERO_ADDRESS:
                    kind = "mint"
                elif dst == ZERO_ADDRESS:
                    kind = "burn"
                else:
                    kind = "transfer"
                parsed.update({"from": src, "to": dst, "token_id": token_id, "erc721_transfer_kind": kind})
        by_tx[tx_hash].append(parsed)
    for events in by_tx.values():
        events.sort(key=lambda e: (e["block_number"], e["transaction_index"], e["log_index"]))
    return dict(by_tx), malformed


def analyze(owner: str, selector: str, evidence: dict[str, dict[str, Any]], logs_by_tx: dict[str, list[dict[str, Any]]], malformed: int) -> dict[str, Any]:
    committed_actions = reverted_actions = 0
    committed_txs = reverted_only_txs = 0
    committed_log_counts: Counter[int] = Counter()
    reverted_with_logs = 0
    topic0_counts: Counter[str] = Counter()
    transfer_kinds: Counter[str] = Counter()
    calldata_lengths: Counter[int] = Counter()
    arg_lengths: Counter[int] = Counter()
    word_counts: Counter[int] = Counter()
    values_nonzero = 0
    msg_senders: Counter[str] = Counter()
    shape_by_position: dict[int, Counter[str]] = defaultdict(Counter)
    word_values_by_position: dict[int, Counter[str]] = defaultdict(Counter)
    tx_rows: dict[str, Any] = {}

    for tx_hash, source in sorted(evidence.items(), key=lambda kv: (kv[1]["block_number"], kv[0])):
        actions = source.get("selector_actions") or []
        committed = [a for a in actions if a.get("source_committed")]
        reverted = [a for a in actions if not a.get("source_committed")]
        committed_actions += len(committed)
        reverted_actions += len(reverted)
        events = logs_by_tx.get(tx_hash, [])
        if committed:
            committed_txs += 1
            committed_log_counts[len(events)] += 1
        else:
            reverted_only_txs += 1
            if events:
                reverted_with_logs += 1
        for action in actions:
            calldata_lengths[int(action["calldata_bytes"])] += 1
            arg_lengths[int(action["argument_bytes"])] += 1
            word_counts[int(action["abi_word_count_ceil"])] += 1
            if int(action.get("value") or 0):
                values_nonzero += 1
            if action.get("msg_sender"):
                msg_senders[str(action["msg_sender"])] += 1
            for idx, shape in enumerate(action.get("abi_word_shapes") or []):
                shape_by_position[idx][str(shape)] += 1
            for idx, value in enumerate(action.get("abi_word_values_hex") or []):
                word_values_by_position[idx][str(value)] += 1
        for event in events:
            topic0_counts[str(event["topic0"])] += 1
            kind = event.get("erc721_transfer_kind")
            if kind:
                transfer_kinds[str(kind)] += 1
        tx_rows[tx_hash] = {**source, "owner_logs": events}

    position_summary = []
    for idx in sorted(set(shape_by_position) | set(word_values_by_position)):
        values = word_values_by_position[idx]
        position_summary.append({
            "word_index": idx,
            "shape_counts": dict(shape_by_position[idx]),
            "distinct_values": len(values),
            "top_values": values.most_common(8),
        })

    return {
        "schema_version": 1,
        "dataset": "vegeta-s1",
        "owner": owner,
        "selector": selector,
        "definition": "owner-scoped selector calls correlated with public owner logs and calldata shape; no concrete storage keys",
        "summary": {
            "selector_transactions": len(evidence),
            "selector_actions": committed_actions + reverted_actions,
            "source_committed_selector_actions": committed_actions,
            "source_reverted_selector_actions": reverted_actions,
            "committed_selector_transactions": committed_txs,
            "reverted_only_selector_transactions": reverted_only_txs,
            "committed_tx_owner_log_count_distribution": {str(k): v for k, v in sorted(committed_log_counts.items())},
            "reverted_selector_tx_with_committed_owner_logs": reverted_with_logs,
            "owner_log_topic0_counts_for_selector_txs": dict(topic0_counts.most_common()),
            "erc721_transfer_effect_counts": dict(transfer_kinds),
            "calldata_byte_length_distribution": {str(k): v for k, v in sorted(calldata_lengths.items())},
            "argument_byte_length_distribution": {str(k): v for k, v in sorted(arg_lengths.items())},
            "abi_word_count_ceil_distribution": {str(k): v for k, v in sorted(word_counts.items())},
            "actions_with_nonzero_msg_value": values_nonzero,
            "distinct_evm_msg_senders": len(msg_senders),
            "top_evm_msg_senders": msg_senders.most_common(10),
            "ignored_malformed_logs": malformed,
            "concrete_storage_keys_used": False,
            "semantic_promotion_performed": False,
        },
        "abi_word_position_summary": position_summary,
        "transactions": tx_rows,
    }


def write_text_report(path: Path, report: dict[str, Any]) -> None:
    s = report["summary"]
    lines = [
        "Vegeta S1 owner-scoped ERC721 selector effect audit",
        "",
        f"owner: {report['owner']}",
        f"selector: {report['selector']}",
        f"selector transactions/actions: {s['selector_transactions']}/{s['selector_actions']}",
        f"source committed/reverted selector actions: {s['source_committed_selector_actions']}/{s['source_reverted_selector_actions']}",
        f"committed/reverted-only selector tx: {s['committed_selector_transactions']}/{s['reverted_only_selector_transactions']}",
        f"committed tx owner-log count distribution: {s['committed_tx_owner_log_count_distribution']}",
        f"reverted selector tx with committed owner logs: {s['reverted_selector_tx_with_committed_owner_logs']}",
        f"owner event topic0 counts: {s['owner_log_topic0_counts_for_selector_txs']}",
        f"ERC721 Transfer effects mint/burn/transfer: {s['erc721_transfer_effect_counts']}",
        f"calldata byte lengths: {s['calldata_byte_length_distribution']}",
        f"argument byte lengths: {s['argument_byte_length_distribution']}",
        f"ABI word-count ceil: {s['abi_word_count_ceil_distribution']}",
        f"actions with nonzero msg.value: {s['actions_with_nonzero_msg_value']}",
        f"distinct EVM msg.senders: {s['distinct_evm_msg_senders']}",
        "",
        "ABI word-position hints (shape counts; distinct values; top values):",
    ]
    for row in report["abi_word_position_summary"]:
        lines.append(
            f"  word[{row['word_index']}] shapes={row['shape_counts']} distinct={row['distinct_values']} top={row['top_values']}"
        )
    lines += [
        "",
        "Interpretation:",
        "  This audit never promotes the selector by itself. Transfer effects can justify a narrow",
        "  event-backed adapter only when they account for the committed behavior; otherwise use the",
        "  calldata shape, event topics, historical implementation/source, and trace context to review it.",
        "  Reverted scopes may count for touched-state coverage but must never create committed native state.",
    ]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--native-plan", type=Path, required=True)
    ap.add_argument("--owner", required=True)
    ap.add_argument("--selector", required=True)
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--logs-json", type=Path, help="offline raw eth_getLogs array for tests/review")
    ap.add_argument("--start-block", type=int, default=16_774_645)
    ap.add_argument("--end-block", type=int, default=16_779_644)
    ap.add_argument("--chunk-blocks", type=int, default=int(os.environ.get("VEGETA_S1_EVENT_LOG_CHUNK_BLOCKS", "250")))
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

    logs_by_tx, malformed = parse_owner_logs(raw_logs, owner)
    report = analyze(owner, selector, evidence, logs_by_tx, malformed)
    report["block_range"] = [ns.start_block, ns.end_block]

    ns.output.parent.mkdir(parents=True, exist_ok=True)
    tmp = ns.output.with_suffix(ns.output.suffix + ".tmp")
    tmp.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(ns.output)
    write_text_report(ns.text_output, report)
    print(ns.text_output.read_text(), end="")
    print(f"JSON: {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
