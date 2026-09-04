#!/usr/bin/env python3
"""Repair committed S1 cw721-drop owner divergences from public ERC-721 Transfer logs.

This is a narrow state-effect recovery pass for ownership only.  It does not replay every NFT
transfer in the S1 window.  Instead it:

* replays prepared cw721-drop owner state offline;
* finds the first translated transfer whose recorded source_owner disagrees with native owner;
* fetches the public Transfer history for only that affected token;
* identifies the minimal currently-divergent suffix of committed source transfers that the native
  plan omitted;
* inserts ordinary ``transfer_nft`` calls into the exact Ethereum transactions that emitted those
  logs, using the event ``from`` owner as the native sender;
* repeats until the full 5,000-block plan has no source/native ownership mismatch.

Using the event owner as sender is intentional: it exercises the contract's normal transfer path
(OWNER, APPROVAL clear, OWNER_COUNT decrement/increment) without adding a privileged force-transfer
message or weakening authorization.  Existing translated transfers are never duplicated.

Safety is fail-closed.  Burns, missing source transactions, ambiguous same-token call ordering,
inconsistent public owner histories, or a mismatch that public logs cannot explain abort without
replacing the execution plan.  No debug_trace, SLOAD/SSTORE keys, or historical storage slots are
used.
"""
from __future__ import annotations

import argparse
import json
import os
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from characterize_vegeta_corpus_compat import RpcClient

TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
ZERO = "0x" + ("00" * 20)
FAMILY = "cw721-drop"
RECOVERY_TAG = "erc721-transfer-log-owner-recovery"


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
    text = str(value or "0")
    return int(text, 16) if text.lower().startswith("0x") else int(text)


def iid_address(iid: str) -> str:
    if ":" not in iid:
        raise RuntimeError(f"invalid cw721-drop instance id: {iid!r}")
    out = norm_addr(iid.split(":", 1)[1])
    if out is None:
        raise RuntimeError(f"cw721-drop instance lacks Ethereum address: {iid!r}")
    return out


def committed_calls(tx: dict):
    if bool(tx.get("source_failed")):
        return
    for ci, call in enumerate(tx.get("calls") or []):
        if call.get("source_revert_scope_action_id") is not None:
            continue
        yield ci, call


def initial_state(manifest: dict):
    families = {str(row.get("instance_id")): str(row.get("family")) for row in manifest.get("instances") or []}
    owner: dict[tuple[str, int], str] = {}
    next_id: dict[str, int] = {}
    for row in manifest.get("instances") or []:
        iid = str(row.get("instance_id") or "")
        if str(row.get("family")) == FAMILY:
            next_id[iid] = int((row.get("instantiate_msg") or {}).get("next_token_id", 1))
    for call in manifest.get("priming_calls") or []:
        iid = str(call.get("instance_id") or "")
        if families.get(iid) != FAMILY:
            continue
        msg = call.get("msg") or {}
        body = msg.get("seed_mint")
        if isinstance(body, dict):
            tid = int(body["token_id"])
            who = norm_addr(body.get("owner"))
            if who is None:
                raise RuntimeError(f"invalid primed cw721 owner: iid={iid} token={tid} owner={body.get('owner')!r}")
            owner[(iid, tid)] = who
            next_id[iid] = max(next_id.get(iid, 1), tid + 1)
    return families, owner, next_id


def represented_transfer_counts(plan: Path) -> Counter:
    counts: Counter = Counter()
    with plan.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            block = json.loads(line)
            for tx in block.get("transactions") or []:
                txh = str(tx.get("tx_hash") or "").lower()
                for _ci, call in committed_calls(tx) or ():
                    if str(call.get("family") or "") != FAMILY:
                        continue
                    body = (call.get("msg") or {}).get("transfer_nft")
                    if not isinstance(body, dict):
                        continue
                    iid = str(call.get("instance_id") or "")
                    tid = int(call.get("source_token_id", body["token_id"]))
                    src = norm_addr(call.get("source_owner"))
                    dst = norm_addr(body.get("recipient"))
                    if src is not None and dst is not None:
                        counts[(iid, tid, txh, src, dst)] += 1
    return counts


def scan_first_owner_gap(plan: Path, manifest: dict) -> dict | None:
    families, owner, next_id = initial_state(manifest)
    with plan.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            block = json.loads(line)
            bn = int(block["block_number"])
            for tx in block.get("transactions") or []:
                txi = int(tx.get("tx_index", -1))
                txh = str(tx.get("tx_hash") or "").lower()
                for ci, call in committed_calls(tx) or ():
                    iid = str(call.get("instance_id") or "")
                    fam = str(call.get("family") or families.get(iid) or "")
                    if fam != FAMILY:
                        continue
                    msg = call.get("msg") or {}
                    if isinstance(msg.get("seed_mint"), dict):
                        body = msg["seed_mint"]
                        tid = int(body["token_id"])
                        who = norm_addr(body.get("owner"))
                        if who is None:
                            raise RuntimeError(f"invalid seed_mint owner: {iid} token={tid}")
                        owner[(iid, tid)] = who
                        next_id[iid] = max(next_id.get(iid, 1), tid + 1)
                        continue
                    if isinstance(msg.get("mint_drop"), dict):
                        body = msg["mint_drop"]
                        q = int(body["quantity"])
                        ids = body.get("token_ids")
                        if ids is None:
                            start = next_id.get(iid, 1)
                            token_ids = list(range(start, start + q))
                            next_id[iid] = start + q
                        else:
                            token_ids = [int(x) for x in ids]
                            if len(token_ids) != q or len(set(token_ids)) != len(token_ids):
                                raise RuntimeError(f"invalid mint_drop token_ids: iid={iid} quantity={q} token_ids={token_ids}")
                            if token_ids:
                                next_id[iid] = max(next_id.get(iid, 1), max(token_ids) + 1)
                        recipient = norm_addr(body.get("recipient"))
                        if recipient is None:
                            raise RuntimeError(f"invalid mint_drop recipient: iid={iid}")
                        for tid in token_ids:
                            owner[(iid, tid)] = recipient
                        continue
                    body = msg.get("transfer_nft")
                    if not isinstance(body, dict):
                        continue
                    tid = int(body["token_id"])
                    current = owner.get((iid, tid))
                    expected = norm_addr(call.get("source_owner"))
                    if expected is not None and current != expected:
                        return {
                            "instance_id": iid,
                            "family": FAMILY,
                            "token_id": tid,
                            "source_token_id": int(call.get("source_token_id", tid)),
                            "native_owner_before_call": current,
                            "source_owner": expected,
                            "location": {
                                "block_number": bn,
                                "transaction_index": txi,
                                "tx_hash": txh,
                                "call_index": ci,
                            },
                        }
                    recipient = norm_addr(body.get("recipient"))
                    if recipient is None:
                        raise RuntimeError(f"invalid transfer recipient: iid={iid} token={tid}")
                    owner[(iid, tid)] = recipient
    return None


def plan_bounds(plan: Path) -> tuple[int, int, int, int]:
    blocks = txs = 0
    first = last = None
    with plan.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            bn = int(row["block_number"])
            first = bn if first is None else min(first, bn)
            last = bn if last is None else max(last, bn)
            blocks += 1
            txs += len(row.get("transactions") or [])
    if first is None or last is None:
        raise RuntimeError("empty execution plan")
    return int(first), int(last), blocks, txs


def token_topic(token_id: int) -> str:
    if token_id < 0 or token_id >= (1 << 256):
        raise RuntimeError(f"ERC721 token ID outside uint256: {token_id}")
    return "0x" + token_id.to_bytes(32, "big").hex()


def fetch_token_logs(client: RpcClient, address: str, token_id: int, start: int, end: int) -> list[dict]:
    out: list[dict] = []
    stack = [(start, end)]
    while stack:
        lo, hi = stack.pop()
        flt = {
            "fromBlock": hex(lo),
            "toBlock": hex(hi),
            "address": address,
            "topics": [TRANSFER_TOPIC, None, None, token_topic(token_id)],
        }
        try:
            rows = client.call("eth_getLogs", [flt]) or []
        except RuntimeError:
            if lo >= hi:
                raise
            mid = (lo + hi) // 2
            stack.append((mid + 1, hi))
            stack.append((lo, mid))
            continue
        if not isinstance(rows, list):
            raise RuntimeError(f"eth_getLogs returned {type(rows).__name__} for token {token_id} blocks {lo}..{hi}")
        out.extend(row for row in rows if isinstance(row, dict))
    return out


def parse_token_logs(rows: list[dict], iid: str, token_id: int) -> list[dict]:
    events = []
    for row in rows:
        topics = row.get("topics") or []
        if len(topics) < 4 or str(topics[0]).lower() != TRANSFER_TOPIC:
            continue
        tid = int_hex(topics[3])
        if tid != token_id:
            continue
        src = norm_addr("0x" + str(topics[1])[-40:])
        dst = norm_addr("0x" + str(topics[2])[-40:])
        if src is None or dst is None:
            raise RuntimeError(f"invalid ERC721 Transfer topics for token {token_id}: {topics}")
        events.append({
            "instance_id": iid,
            "token_id": token_id,
            "from": src,
            "to": dst,
            "block_number": int_hex(row.get("blockNumber")),
            "transaction_index": int_hex(row.get("transactionIndex")),
            "log_index": int_hex(row.get("logIndex")),
            "tx_hash": str(row.get("transactionHash") or "").lower(),
        })
    events.sort(key=lambda e: (e["block_number"], e["transaction_index"], e["log_index"]))
    return events


def before_gap(event: dict, gap: dict) -> bool:
    loc = gap["location"]
    eb = int(event["block_number"])
    et = int(event["transaction_index"])
    gb = int(loc["block_number"])
    gt = int(loc["transaction_index"])
    return eb < gb or (eb == gb and et < gt)


def choose_recovery_events(
    events: list[dict],
    gap: dict,
    initial_owner: str | None,
    represented: Counter,
) -> list[dict]:
    """Return the minimal currently-divergent suffix of missing public transfers before ``gap``."""
    source = initial_owner
    native = initial_owner
    outstanding: list[dict] = []
    local = Counter(represented)

    for event in events:
        if not before_gap(event, gap):
            continue
        src, dst = event["from"], event["to"]
        if src == ZERO:
            if source is not None:
                raise RuntimeError(
                    f"public mint recreates existing token before owner gap: iid={gap['instance_id']} "
                    f"token={gap['source_token_id']} owner={source} event={event}"
                )
            source = dst
            native = dst
            outstanding = []
            continue
        if dst == ZERO:
            raise RuntimeError(
                f"ERC721 burn encountered before required owner repair; no burn analogue is defined: {event}"
            )
        if source != src:
            raise RuntimeError(
                "public ERC721 Transfer history is not owner-contiguous before gap: "
                f"expected_from={source} event={event} gap={gap}"
            )
        source = dst
        ident = (gap["instance_id"], int(gap["source_token_id"]), event["tx_hash"], src, dst)
        if local[ident] > 0:
            local[ident] -= 1
            if native != src:
                raise RuntimeError(
                    "translated transfer appears after an unrecovered ownership divergence; "
                    f"event={event} native_owner={native} gap={gap}"
                )
            native = dst
        else:
            outstanding.append(event)
        if source == native:
            outstanding = []

    expected_source = gap.get("source_owner")
    current_native = gap.get("native_owner_before_call")
    if source != expected_source:
        raise RuntimeError(
            "public Transfer history does not produce the source_owner recorded by the translated call: "
            f"iid={gap['instance_id']} token={gap['source_token_id']} logs_owner={source} "
            f"translated_source_owner={expected_source} gap={gap}"
        )
    if native != current_native:
        raise RuntimeError(
            "public/translated transfer replay does not reproduce native owner at gap: "
            f"iid={gap['instance_id']} token={gap['source_token_id']} replay_native={native} "
            f"scan_native={current_native} gap={gap}"
        )
    if source == native:
        raise RuntimeError(f"owner gap unexpectedly closes without missing public transfers: {gap}")
    if not outstanding:
        raise RuntimeError(f"owner gap has no recoverable missing public transfer suffix: {gap}")
    return outstanding


def recovery_call(event: dict) -> dict:
    return {
        "kind": "execute",
        "family": FAMILY,
        "instance_id": event["instance_id"],
        "sender": event["from"],
        "msg": {"transfer_nft": {"recipient": event["to"], "token_id": int(event["token_id"])}},
        "origin_action_id": None,
        "origin_selector": None,
        "source_owner": event["from"],
        "source_token_id": int(event["token_id"]),
        "source_effect_recovery": RECOVERY_TAG,
        "source_effect_tx_hash": event["tx_hash"],
        "source_effect_log_index": int(event["log_index"]),
    }


def inject_events(src: Path, dst: Path, events: list[dict]) -> int:
    by_tx: dict[str, list[dict]] = defaultdict(list)
    for event in events:
        by_tx[event["tx_hash"]].append(event)
    for rows in by_tx.values():
        rows.sort(key=lambda e: int(e["log_index"]))

    found: set[str] = set()
    injected = 0
    with src.open(encoding="utf-8") as inp, dst.open("w", encoding="utf-8") as out:
        for line in inp:
            if not line.strip():
                continue
            block = json.loads(line)
            for tx in block.get("transactions") or []:
                txh = str(tx.get("tx_hash") or "").lower()
                rows = by_tx.get(txh)
                if not rows:
                    continue
                found.add(txh)
                if bool(tx.get("source_failed")):
                    raise RuntimeError(f"committed ERC721 Transfer log maps to source-failed plan tx: {txh}")
                calls = list(tx.get("calls") or [])
                for event in rows:
                    for call in calls:
                        if str(call.get("family") or "") != FAMILY or str(call.get("instance_id") or "") != event["instance_id"]:
                            continue
                        msg = call.get("msg") or {}
                        token_body = msg.get("transfer_nft") or msg.get("approve_nft") or msg.get("owner_of") or msg.get("approved")
                        if isinstance(token_body, dict) and token_body.get("token_id") is not None:
                            call_tid = int(call.get("source_token_id", token_body["token_id"]))
                            if call_tid == int(event["token_id"]):
                                raise RuntimeError(
                                    "missing public transfer shares a source transaction with another native call on the same "
                                    "cw721 token; call/log relative ordering is ambiguous, refusing automatic insertion: "
                                    f"tx={txh} event={event} call={call}"
                                )
                tx["calls"] = [recovery_call(event) for event in rows] + calls
                injected += len(rows)
            out.write(json.dumps(block, separators=(",", ":")) + "\n")
    missing = sorted(set(by_tx) - found)
    if missing:
        raise RuntimeError(f"public ERC721 transfer source transactions missing from execution plan: {missing[:20]}")
    return injected


def write_report(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)
    text = path.with_suffix(".txt")
    lines = [
        "Vegeta S1 CW721 ownership recovery",
        "",
        f"status: {data['status']}",
        f"blocks: {data['blocks']}",
        f"transactions: {data['transactions']}",
        f"owner gaps repaired: {data['owner_gaps_repaired']}",
        f"public transfer effects injected: {data['injected_calls']}",
        f"affected tokens: {len(data['affected_tokens'])}",
        f"remaining owner gaps: {data['remaining_owner_gaps']}",
        "",
        "Method: token-scoped public ERC721 Transfer logs; no debug_trace/SLOAD/SSTORE/storage-key oracle.",
        "Only the divergent suffix required to close an observed owner mismatch is materialized.",
    ]
    text.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--execution-plan", type=Path, required=True)
    ap.add_argument("--manifest", type=Path, required=True)
    ap.add_argument("--report", type=Path, required=True)
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--max-repairs", type=int, default=100)
    ns = ap.parse_args()
    if ns.max_repairs <= 0:
        raise SystemExit("--max-repairs must be positive")
    if not ns.execution_plan.is_file() or not ns.manifest.is_file():
        raise SystemExit("missing execution plan/manifest")

    manifest = json.loads(ns.manifest.read_text(encoding="utf-8"))
    start, end, blocks, txs = plan_bounds(ns.execution_plan)
    if blocks != int(manifest.get("blocks", -1)) or txs != int(manifest.get("transactions", -1)):
        raise SystemExit("execution plan domain disagrees with manifest")

    first_gap = scan_first_owner_gap(ns.execution_plan, manifest)
    if first_gap is None:
        data = {
            "schema_version": 1,
            "status": "already-complete",
            "blocks": blocks,
            "transactions": txs,
            "owner_gaps_repaired": 0,
            "injected_calls": 0,
            "affected_tokens": [],
            "remaining_owner_gaps": 0,
            "public_transfer_log_queries": 0,
        }
        write_report(ns.report, data)
        print("PASS: full-domain committed cw721-drop ownership already source-consistent; no RPC recovery needed")
        return 0
    if not ns.rpc_url:
        raise SystemExit(
            "cw721-drop ownership divergence requires token-scoped public Transfer logs, but ETH_RPC_URL is unset: "
            + json.dumps(first_gap, sort_keys=True)
        )

    print(
        "recovering S1 cw721-drop ownership via token-scoped public Transfer logs: "
        f"first_gap={first_gap['location']['block_number']}/{first_gap['location']['transaction_index']}/"
        f"{first_gap['location']['call_index']} instance={first_gap['instance_id']} "
        f"token={first_gap['source_token_id']} native_owner={first_gap['native_owner_before_call']} "
        f"source_owner={first_gap['source_owner']}",
        flush=True,
    )

    client = RpcClient(ns.rpc_url, timeout=60, retries=5, backoff=1.0)
    tmp = ns.execution_plan.with_suffix(ns.execution_plan.suffix + ".owner-repair.tmp")
    work = ns.execution_plan
    owned_tmp = False
    log_cache: dict[tuple[str, int], list[dict]] = {}
    affected: set[tuple[str, int]] = set()
    total_injected = 0
    gaps_repaired = 0
    queries = 0

    try:
        for _iteration in range(1, ns.max_repairs + 1):
            gap = scan_first_owner_gap(work, manifest)
            if gap is None:
                break
            iid = gap["instance_id"]
            tid = int(gap["source_token_id"])
            key = (iid, tid)
            affected.add(key)
            if key not in log_cache:
                raw = fetch_token_logs(client, iid_address(iid), tid, start, end)
                log_cache[key] = parse_token_logs(raw, iid, tid)
                queries += 1
                print(
                    f"ownership token logs instance={iid} token={tid} events={len(log_cache[key])}",
                    flush=True,
                )
            represented = represented_transfer_counts(work)
            _families, initial_owner_map, _next = initial_state(manifest)
            initial = initial_owner_map.get((iid, int(gap["token_id"])))
            needed = choose_recovery_events(log_cache[key], gap, initial, represented)
            print(
                f"owner gap repair instance={iid} token={tid} recovery_events={len(needed)} "
                f"first_use={gap['location']['block_number']}/{gap['location']['transaction_index']}/"
                f"{gap['location']['call_index']}",
                flush=True,
            )
            next_tmp = ns.execution_plan.with_suffix(ns.execution_plan.suffix + f".owner-repair.{_iteration}.tmp")
            injected = inject_events(work, next_tmp, needed)
            if owned_tmp:
                Path(work).unlink(missing_ok=True)
            work = next_tmp
            owned_tmp = True
            total_injected += injected
            gaps_repaired += 1
        else:
            raise RuntimeError(f"ownership repair exceeded --max-repairs={ns.max_repairs}")

        remaining = scan_first_owner_gap(work, manifest)
        if remaining is not None:
            raise RuntimeError(f"ownership repair did not close full-domain mismatch: {json.dumps(remaining, sort_keys=True)}")

        if owned_tmp:
            Path(work).replace(tmp)
            tmp.replace(ns.execution_plan)
            stats = manifest.setdefault("statistics", {})
            stats["event_recovered_nft_transfer_calls"] = int(
                stats.get("event_recovered_nft_transfer_calls", 0)
            ) + total_injected
            stats["contract_calls"] = int(stats.get("contract_calls", 0)) + total_injected
            norm = manifest.setdefault("normalization", {})
            norm["nft_ownership_effect_recovery"] = {
                "mode": "token-scoped-public-erc721-transfer",
                "owner_gaps_repaired": gaps_repaired,
                "injected_calls": total_injected,
                "affected_tokens": [
                    {"instance_id": iid, "token_id": tid} for iid, tid in sorted(affected)
                ],
                "concrete_storage_keys_used": False,
                "debug_trace_used": False,
                "policy": (
                    "materialize only the currently-divergent suffix of public Transfer effects "
                    "required to reconcile an observed translated source_owner with native owner state"
                ),
            }
            ns.manifest.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        data = {
            "schema_version": 1,
            "status": "repaired-and-validated" if total_injected else "already-complete",
            "blocks": blocks,
            "transactions": txs,
            "owner_gaps_repaired": gaps_repaired,
            "injected_calls": total_injected,
            "affected_tokens": [
                {"instance_id": iid, "token_id": tid} for iid, tid in sorted(affected)
            ],
            "remaining_owner_gaps": 0,
            "public_transfer_log_queries": queries,
            "concrete_storage_keys_used": False,
            "debug_trace_used": False,
            "recovery_policy": "minimal divergent public Transfer suffix per observed source/native owner mismatch",
        }
        write_report(ns.report, data)
        print(
            "PASS: repaired full-domain committed cw721-drop ownership from public Transfer logs: "
            f"owner_gaps={gaps_repaired} injected={total_injected} affected_tokens={len(affected)} remaining=0"
        )
        return 0
    except Exception:
        if owned_tmp:
            Path(work).unlink(missing_ok=True)
        tmp.unlink(missing_ok=True)
        raise


if __name__ == "__main__":
    raise SystemExit(main())
