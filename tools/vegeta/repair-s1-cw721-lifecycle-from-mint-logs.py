#!/usr/bin/env python3
"""Repair missing committed S1 ERC-721 creation effects from public Transfer(from=0) logs.

This is intentionally narrow.  It first simulates the prepared native NFT lifecycle entirely
offline.  Only cw721 instances with a committed token-specific call whose token does not exist are
eligible.  For those instances, the script fetches public ERC-721 mint logs over the frozen S1
window and injects admin SeedMint/Mint calls into the exact source transaction that emitted each
mint.  It never uses debug_trace, SLOAD/SSTORE keys, or historical storage slots.

Safety rules are fail-closed:
* source-failed transactions and caught internal revert scopes do not affect committed lifecycle;
* translated cw721-drop mints on affected instances are reconciled to exact public event token ids;
* translated drop quantity/recipient must exactly match the public mint events in its source tx;
* public mint transactions without a translated mint_drop are ignored unless they contain a required
  missing-token recovery event;
* every missing token must have exactly one earlier committed zero-address Transfer mint event;
* a mint event may not recreate a token that was already present in predecessor-state priming;
* the rewritten 5,000-block plan is simulated again and must have zero committed NFT gaps.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from collections import defaultdict
from pathlib import Path
from typing import Any

from characterize_vegeta_corpus_compat import RpcClient

TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
ZERO_ADDRESS_TOPIC = "0x" + ("00" * 32)
NFT_FAMILIES = {"cw721-drop", "cw721-mintable"}


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


def iid_address(iid: str) -> str:
    if ":" not in iid:
        raise ValueError(f"invalid NFT instance_id: {iid!r}")
    address = norm_addr(iid.split(":", 1)[1])
    if address is None:
        raise ValueError(f"NFT instance_id lacks Ethereum address: {iid!r}")
    return address


def token_ref(call: dict) -> tuple[str, int] | None:
    msg = call.get("msg") or {}
    for op in ("transfer_nft", "approve_nft", "owner_of", "approved"):
        body = msg.get(op)
        if isinstance(body, dict) and body.get("token_id") is not None:
            return op, int(body["token_id"])
    return None


def committed_calls(tx: dict):
    if bool(tx.get("source_failed")):
        return
    for i, call in enumerate(tx.get("calls") or []):
        if call.get("source_revert_scope_action_id") is not None:
            continue
        yield i, call


def initial_nft_state(manifest: dict):
    family = {str(row["instance_id"]): str(row["family"]) for row in manifest.get("instances") or []}
    exists: dict[str, set[int]] = defaultdict(set)
    next_id: dict[str, int] = {}
    for row in manifest.get("instances") or []:
        iid = str(row["instance_id"])
        fam = str(row["family"])
        if fam == "cw721-drop":
            next_id[iid] = int((row.get("instantiate_msg") or {}).get("next_token_id", 1))
    for call in manifest.get("priming_calls") or []:
        iid = str(call.get("instance_id") or "")
        fam = family.get(iid)
        if fam not in NFT_FAMILIES:
            continue
        msg = call.get("msg") or {}
        if fam == "cw721-drop" and isinstance(msg.get("seed_mint"), dict):
            tid = int(msg["seed_mint"]["token_id"])
            exists[iid].add(tid)
            next_id[iid] = max(next_id.get(iid, 1), tid + 1)
        elif fam == "cw721-mintable" and isinstance(msg.get("mint"), dict):
            exists[iid].add(int(msg["mint"]["token_id"]))
    return family, exists, next_id


def scan_plan(plan: Path, manifest: dict) -> dict:
    family, exists, next_id = initial_nft_state(manifest)
    initial_exists = {iid: set(tokens) for iid, tokens in exists.items()}
    gaps: dict[tuple[str, int], dict] = {}
    translated_mint_instances: set[str] = set()
    blocks = txs = 0
    first_block = last_block = None

    with plan.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            block = json.loads(line)
            bn = int(block["block_number"])
            blocks += 1
            first_block = bn if first_block is None else min(first_block, bn)
            last_block = bn if last_block is None else max(last_block, bn)
            for tx in block.get("transactions") or []:
                txs += 1
                for call_index, call in committed_calls(tx) or ():
                    iid = str(call.get("instance_id") or "")
                    fam = str(call.get("family") or family.get(iid) or "")
                    if fam not in NFT_FAMILIES or not iid:
                        continue
                    msg = call.get("msg") or {}
                    if fam == "cw721-drop" and isinstance(msg.get("seed_mint"), dict):
                        tid = int(msg["seed_mint"]["token_id"])
                        exists[iid].add(tid)
                        next_id[iid] = max(next_id.get(iid, 1), tid + 1)
                        if call.get("source_effect_recovery") != "erc721-transfer-mint-log":
                            translated_mint_instances.add(iid)
                        continue
                    if fam == "cw721-mintable" and isinstance(msg.get("mint"), dict):
                        exists[iid].add(int(msg["mint"]["token_id"]))
                        if call.get("source_effect_recovery") != "erc721-transfer-mint-log":
                            translated_mint_instances.add(iid)
                        continue
                    if fam == "cw721-drop" and isinstance(msg.get("mint_drop"), dict):
                        translated_mint_instances.add(iid)
                        body = msg["mint_drop"]
                        q = int(body["quantity"])
                        raw_ids = body.get("token_ids")
                        if raw_ids is None:
                            start = next_id.get(iid, 1)
                            token_ids = list(range(start, start + q))
                            next_id[iid] = start + q
                        else:
                            token_ids = [int(value) for value in raw_ids]
                            if len(token_ids) != q or len(set(token_ids)) != len(token_ids):
                                raise RuntimeError(
                                    f"invalid exact token_ids on {iid}: quantity={q} token_ids={token_ids}"
                                )
                            if token_ids:
                                next_id[iid] = max(next_id.get(iid, 1), max(token_ids) + 1)
                        exists[iid].update(token_ids)
                        continue
                    ref = token_ref(call)
                    if ref is None:
                        continue
                    op, tid = ref
                    if tid not in exists[iid]:
                        key = (iid, tid)
                        gaps.setdefault(key, {
                            "instance_id": iid,
                            "family": fam,
                            "token_id": tid,
                            "first_use": {
                                "block_number": bn,
                                "transaction_index": int(tx["tx_index"]),
                                "tx_hash": str(tx.get("tx_hash") or "").lower(),
                                "call_index": call_index,
                                "operation": op,
                                "source_owner": call.get("source_owner"),
                            },
                        })
    return {
        "blocks": blocks,
        "transactions": txs,
        "first_block": first_block,
        "last_block": last_block,
        "gaps": gaps,
        "affected_instances": sorted({iid for iid, _ in gaps}),
        "translated_mint_instances": translated_mint_instances,
        "initial_exists": initial_exists,
    }


def fetch_mint_logs(client: RpcClient, addresses: list[str], start: int, end: int, chunk: int) -> list[dict]:
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
                "address": addresses if len(addresses) > 1 else addresses[0],
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
        print(f"lifecycle mint logs blocks={current}..{hi} events={len(out)}", flush=True)
        current = hi + 1
    return out


def parse_logs(rows: list[dict], iid_by_address: dict[str, str]) -> list[dict]:
    events = []
    for row in rows:
        address = norm_addr(row.get("address"))
        iid = iid_by_address.get(address or "")
        topics = row.get("topics") or []
        if iid is None or len(topics) < 4:
            continue
        recipient_topic = str(topics[2] or "").lower().replace("0x", "")
        recipient = norm_addr("0x" + recipient_topic[-40:])
        if recipient is None:
            continue
        events.append({
            "instance_id": iid,
            "contract_address": address,
            "block_number": int_hex(row.get("blockNumber")),
            "transaction_index": int_hex(row.get("transactionIndex")),
            "log_index": int_hex(row.get("logIndex")),
            "tx_hash": str(row.get("transactionHash") or "").lower(),
            "token_id": int_hex(topics[3]),
            "recipient": recipient,
        })
    events.sort(key=lambda x: (x["block_number"], x["transaction_index"], x["log_index"]))
    return events


def event_is_before_use(event: dict, use: dict) -> bool:
    return (event["block_number"], event["transaction_index"]) <= (use["block_number"], use["transaction_index"])


def validate_recovery(scan: dict, events: list[dict]) -> dict[tuple[str, int], dict]:
    by_token: dict[tuple[str, int], list[dict]] = defaultdict(list)
    for event in events:
        by_token[(event["instance_id"], int(event["token_id"]))].append(event)
    errors = []
    required: dict[tuple[str, int], dict] = {}
    for key, gap in sorted(scan["gaps"].items()):
        eligible = [e for e in by_token.get(key, []) if event_is_before_use(e, gap["first_use"])]
        if len(eligible) != 1:
            errors.append({"gap": gap, "eligible_mint_events": eligible})
        else:
            required[key] = eligible[0]
    if errors:
        sample = errors[:10]
        raise RuntimeError(
            "cannot safely recover every missing NFT token from a unique earlier public mint event; "
            f"unresolved={len(errors)} sample={json.dumps(sample, sort_keys=True)}"
        )

    duplicate_mints = [rows for rows in by_token.values() if len(rows) > 1]
    if duplicate_mints:
        raise RuntimeError(
            "public mint logs reuse an ERC721 token id within the S1 window; burn/remint lifecycle "
            f"requires explicit review. sample={duplicate_mints[:10]}"
        )

    recreated = []
    for event in events:
        if int(event["token_id"]) in scan["initial_exists"].get(event["instance_id"], set()):
            recreated.append(event)
    if recreated:
        raise RuntimeError(
            "public in-window mint event reuses a token seeded from predecessor state; burn/remint lifecycle "
            f"requires explicit review. sample={recreated[:10]}"
        )
    return required


def recovery_call(family: str, iid: str, event: dict) -> dict:
    if family == "cw721-drop":
        msg = {"seed_mint": {"owner": event["recipient"], "token_id": int(event["token_id"])}}
    elif family == "cw721-mintable":
        msg = {"mint": {"owner": event["recipient"], "token_id": int(event["token_id"]), "token_uri": None}}
    else:
        raise ValueError(f"unsupported recovery family {family!r}")
    return {
        "kind": "execute",
        "family": family,
        "instance_id": iid,
        "msg": msg,
        "origin_action_id": None,
        "origin_selector": None,
        "sender": "native-s3-admin",
        "source_effect_recovery": "erc721-transfer-mint-log",
        "source_effect_tx_hash": event["tx_hash"],
        "source_effect_log_index": int(event["log_index"]),
    }


def exactify_drop_calls_for_tx(calls: list[dict], iid: str, events: list[dict], tx_hash: str) -> tuple[int, int]:
    """Bind committed mint_drop quantities to exact ERC-721 mint events in source log order.

    The outer mint_drop action stays unchanged, so wallet/stage/nonce dependencies are preserved.
    Only token allocation changes from the synthetic sequential ids to the source event ids.
    """
    drop_calls: list[dict] = []
    for call in calls:
        if call.get("source_revert_scope_action_id") is not None:
            continue
        if str(call.get("instance_id") or "") != iid:
            continue
        body = (call.get("msg") or {}).get("mint_drop")
        if isinstance(body, dict):
            drop_calls.append(call)
    if not drop_calls:
        if events:
            raise RuntimeError(
                f"public mint events exist for translated-mint instance {iid} tx={tx_hash} but no committed mint_drop call"
            )
        return 0, 0

    expected_quantity = sum(int(call["msg"]["mint_drop"]["quantity"]) for call in drop_calls)
    if expected_quantity != len(events):
        raise RuntimeError(
            "translated mint_drop/public Transfer(from=0) quantity mismatch while exactifying token ids: "
            f"instance={iid} tx={tx_hash} translated_quantity={expected_quantity} public_events={len(events)} "
            f"event_token_ids={[int(e['token_id']) for e in events]}"
        )

    cursor = 0
    exactified_calls = exactified_tokens = 0
    for call in drop_calls:
        body = call["msg"]["mint_drop"]
        quantity = int(body["quantity"])
        chunk = events[cursor: cursor + quantity]
        cursor += quantity
        recipient = norm_addr(body.get("recipient"))
        event_recipients = [norm_addr(event.get("recipient")) for event in chunk]
        if recipient is None or any(value != recipient for value in event_recipients):
            raise RuntimeError(
                "translated mint_drop/public Transfer(from=0) recipient/order mismatch while exactifying token ids: "
                f"instance={iid} tx={tx_hash} call_recipient={body.get('recipient')} "
                f"event_recipients={event_recipients} token_ids={[int(e['token_id']) for e in chunk]}"
            )
        token_ids = [int(event["token_id"]) for event in chunk]
        if len(set(token_ids)) != len(token_ids):
            raise RuntimeError(f"duplicate public mint token ids in {iid} tx={tx_hash}: {token_ids}")
        body["token_ids"] = token_ids
        call["source_token_id_recovery"] = "erc721-transfer-mint-log"
        call["source_token_id_event_log_indices"] = [int(event["log_index"]) for event in chunk]
        exactified_calls += 1
        exactified_tokens += quantity
    return exactified_calls, exactified_tokens


def rewrite_plan(plan: Path, manifest_path: Path, manifest: dict, scan: dict, events: list[dict], required: dict[tuple[str, int], dict]) -> dict:
    family = {str(row["instance_id"]): str(row["family"]) for row in manifest.get("instances") or []}
    overlap = set(scan["affected_instances"]) & set(scan["translated_mint_instances"])

    events_by_tx_iid: dict[tuple[str, str], list[dict]] = defaultdict(list)
    all_events_by_tx: dict[str, list[dict]] = defaultdict(list)
    for event in events:
        events_by_tx_iid[(event["tx_hash"], event["instance_id"])].append(event)
        all_events_by_tx[event["tx_hash"]].append(event)
    for rows in events_by_tx_iid.values():
        rows.sort(key=lambda x: x["log_index"])
    for rows in all_events_by_tx.values():
        rows.sort(key=lambda x: x["log_index"])

    # Public logs are an oracle for exact token allocation, not a request to translate every mint
    # on an affected collection.  Reconcile every committed translated mint_drop on an overlap
    # instance.  A public mint tx with no translated mint_drop is only materialized when that exact
    # event is required to close one of the lifecycle gaps discovered by the offline scan.
    required_event_keys = {
        (event["tx_hash"], event["instance_id"], int(event["token_id"]), int(event["log_index"]))
        for event in required.values()
    }
    required_events_by_tx: dict[str, list[dict]] = defaultdict(list)
    for event in events:
        key = (event["tx_hash"], event["instance_id"], int(event["token_id"]), int(event["log_index"]))
        if key in required_event_keys:
            required_events_by_tx[event["tx_hash"]].append(event)
    for rows in required_events_by_tx.values():
        rows.sort(key=lambda x: x["log_index"])

    tmp = plan.with_suffix(plan.suffix + ".lifecycle-repair.tmp")
    inserted = exactified_calls = exactified_tokens = 0
    found_event_txs: set[str] = set()
    found_required_event_keys: set[tuple[str, str, int, int]] = set()
    exactified_tx_iids: set[tuple[str, str]] = set()
    translated_drop_tx_iids: set[tuple[str, str]] = set()
    with plan.open(encoding="utf-8") as src, tmp.open("w", encoding="utf-8") as dst:
        for line in src:
            if not line.strip():
                continue
            block = json.loads(line)
            for tx in block.get("transactions") or []:
                tx_hash = str(tx.get("tx_hash") or "").lower()
                calls = tx.setdefault("calls", [])

                # Identify committed translated MintDrop calls from the plan itself.  This is the
                # transaction-level distinction the previous instance-level overlap logic lacked.
                drop_iids: set[str] = set()
                if not bool(tx.get("source_failed")):
                    for call in calls:
                        if call.get("source_revert_scope_action_id") is not None:
                            continue
                        iid = str(call.get("instance_id") or "")
                        if iid not in overlap:
                            continue
                        body = (call.get("msg") or {}).get("mint_drop")
                        if isinstance(body, dict):
                            drop_iids.add(iid)
                            translated_drop_tx_iids.add((tx_hash, iid))

                tx_events = all_events_by_tx.get(tx_hash, [])
                if tx_events:
                    if bool(tx.get("source_failed")):
                        tmp.unlink(missing_ok=True)
                        raise RuntimeError(f"committed ERC721 mint log belongs to source_failed tx {tx_hash}")
                    found_event_txs.add(tx_hash)

                for iid in sorted(drop_iids):
                    rows = events_by_tx_iid.get((tx_hash, iid), [])
                    if not rows:
                        tmp.unlink(missing_ok=True)
                        raise RuntimeError(
                            "committed translated mint_drop on affected instance has no public "
                            f"Transfer(from=0) events: instance={iid} tx={tx_hash}"
                        )
                    c, n = exactify_drop_calls_for_tx(calls, iid, rows, tx_hash)
                    exactified_calls += c
                    exactified_tokens += n
                    exactified_tx_iids.add((tx_hash, iid))

                additions = []
                existing_recovery = {
                    (
                        str(c.get("instance_id") or ""),
                        int(((c.get("msg") or {}).get("seed_mint") or (c.get("msg") or {}).get("mint") or {}).get("token_id", -1)),
                    )
                    for c in calls if c.get("source_effect_recovery") == "erc721-transfer-mint-log"
                }
                for event in required_events_by_tx.get(tx_hash, []):
                    event_key = (
                        event["tx_hash"], event["instance_id"], int(event["token_id"]), int(event["log_index"])
                    )
                    found_required_event_keys.add(event_key)

                    # If this source tx already has a translated MintDrop for the same instance,
                    # exactification above materializes the event and preserves mint bookkeeping.
                    if event["instance_id"] in drop_iids:
                        continue

                    key = (event["instance_id"], int(event["token_id"]))
                    if key in existing_recovery:
                        continue
                    additions.append(recovery_call(family[event["instance_id"]], event["instance_id"], event))
                    inserted += 1
                if additions:
                    # These are exact missing source effects.  SeedMint advances next_token_id, but
                    # every translated MintDrop on an affected overlap instance is exactified above,
                    # so later token allocation does not depend on that synthetic sequence.
                    tx["calls"] = additions + calls
            dst.write(json.dumps(block, separators=(",", ":")) + "\n")

    missing_txs = sorted(set(all_events_by_tx) - found_event_txs)
    if missing_txs:
        tmp.unlink(missing_ok=True)
        raise RuntimeError(f"mint-event transactions missing from execution plan: {missing_txs[:20]}")

    missing_required = sorted(required_event_keys - found_required_event_keys)
    if missing_required:
        tmp.unlink(missing_ok=True)
        raise RuntimeError(f"required mint recovery events missing from execution plan: {missing_required[:20]}")

    missing_exactified = sorted(translated_drop_tx_iids - exactified_tx_iids)
    if missing_exactified:
        tmp.unlink(missing_ok=True)
        raise RuntimeError(f"unreconciled translated mint_drop transactions remain: {missing_exactified[:20]}")

    # Exactifying a translated mint_drop can expose a token that the old synthetic sequential
    # allocator happened to create by accident.  Such a token was not visible in the original gap
    # set, so it could not have been part of `required` above.  Re-scan once after exactification and
    # initial recovery, then materialize any newly exposed source mint effects from the *same* public
    # log set.  No additional RPC is needed.
    after = scan_plan(tmp, manifest)
    supplemental_required: dict[tuple[str, int], dict] = {}
    if after["gaps"]:
        supplemental_required = validate_recovery(after, events)
        print(
            "post-exactification lifecycle closure exposed supplemental gaps: "
            f"gaps={len(after['gaps'])} recovery_events={len(supplemental_required)}",
            flush=True,
        )

        supplemental_by_tx: dict[str, list[dict]] = defaultdict(list)
        for event in supplemental_required.values():
            supplemental_by_tx[event["tx_hash"]].append(event)
        for rows in supplemental_by_tx.values():
            rows.sort(key=lambda x: x["log_index"])

        second = tmp.with_suffix(tmp.suffix + ".supplemental")
        found_supplemental: set[tuple[str, int]] = set()
        with tmp.open(encoding="utf-8") as src, second.open("w", encoding="utf-8") as dst:
            for line in src:
                if not line.strip():
                    continue
                block = json.loads(line)
                for tx in block.get("transactions") or []:
                    tx_hash = str(tx.get("tx_hash") or "").lower()
                    rows = supplemental_by_tx.get(tx_hash, [])
                    if not rows:
                        continue
                    if bool(tx.get("source_failed")):
                        second.unlink(missing_ok=True)
                        tmp.unlink(missing_ok=True)
                        raise RuntimeError(f"supplemental ERC721 mint recovery belongs to source_failed tx {tx_hash}")

                    calls = tx.setdefault("calls", [])
                    additions = []
                    existing_recovery = {
                        (
                            str(c.get("instance_id") or ""),
                            int(((c.get("msg") or {}).get("seed_mint") or (c.get("msg") or {}).get("mint") or {}).get("token_id", -1)),
                        )
                        for c in calls if c.get("source_effect_recovery") == "erc721-transfer-mint-log"
                    }
                    for event in rows:
                        key = (event["instance_id"], int(event["token_id"]))
                        found_supplemental.add(key)

                        # A token emitted by a translated MintDrop transaction should already exist
                        # after exactification.  If it does not, adding SeedMint would hide a genuine
                        # reconciliation bug, so keep this case fail-closed.
                        translated_here = any(
                            call.get("source_revert_scope_action_id") is None
                            and str(call.get("instance_id") or "") == event["instance_id"]
                            and isinstance((call.get("msg") or {}).get("mint_drop"), dict)
                            for call in calls
                        )
                        if translated_here:
                            second.unlink(missing_ok=True)
                            tmp.unlink(missing_ok=True)
                            raise RuntimeError(
                                "post-exactification lifecycle gap points at a translated mint_drop tx; "
                                "exact token reconciliation should already have materialized it: "
                                f"instance={event['instance_id']} token_id={event['token_id']} tx={tx_hash}"
                            )
                        if key in existing_recovery:
                            continue
                        additions.append(recovery_call(family[event["instance_id"]], event["instance_id"], event))
                        inserted += 1
                    if additions:
                        tx["calls"] = additions + calls
                dst.write(json.dumps(block, separators=(",", ":")) + "\n")

        missing_supplemental = sorted(set(supplemental_required) - found_supplemental)
        if missing_supplemental:
            second.unlink(missing_ok=True)
            tmp.unlink(missing_ok=True)
            raise RuntimeError(
                f"supplemental mint recovery transactions missing from execution plan: {missing_supplemental[:20]}"
            )
        second.replace(tmp)
        after = scan_plan(tmp, manifest)

    if after["gaps"]:
        sample = list(after["gaps"].values())[:10]
        tmp.unlink(missing_ok=True)
        raise RuntimeError(
            f"lifecycle repair did not close full-domain gaps: remaining={len(after['gaps'])} "
            f"sample={json.dumps(sample, sort_keys=True)}"
        )

    tmp.replace(plan)
    if exactified_calls:
        wasm = manifest.setdefault("wasm_artifacts", {})
        wasm["cw721-drop"] = (
            "benchmarks/target/wasm32-unknown-unknown/release/"
            "acg_benchmark_native_s1_cw721_drop_exact.wasm"
        )

    stats = manifest.setdefault("statistics", {})
    stats["event_recovered_nft_mint_calls"] = int(stats.get("event_recovered_nft_mint_calls", 0)) + inserted
    stats["event_exactified_nft_mint_drop_calls"] = int(stats.get("event_exactified_nft_mint_drop_calls", 0)) + exactified_calls
    stats["contract_calls"] = int(stats.get("contract_calls", 0)) + inserted
    norm = manifest.setdefault("normalization", {})
    norm["nft_lifecycle_effect_recovery"] = {
        "mode": "public-erc721-transfer-from-zero",
        "affected_instances": scan["affected_instances"],
        "translated_mint_overlap_instances": sorted(overlap),
        "observed_public_mint_events": len(events),
        "required_gap_mint_events": len(required),
        "supplemental_post_exactification_mint_events": len(supplemental_required),
        "total_required_mint_events": len(set(required) | set(supplemental_required)),
        "injected_calls": inserted,
        "exactified_mint_drop_calls": exactified_calls,
        "exactified_mint_drop_tokens": exactified_tokens,
        "cw721_drop_wasm_artifact": manifest.get("wasm_artifacts", {}).get("cw721-drop"),
        "concrete_storage_keys_used": False,
        "debug_trace_used": False,
        "policy": (
            "translated mint_drop transactions on overlap cw721-drop instances retain mint_drop semantics "
            "with exact public token_ids; untranslated public mint transactions are materialized only for "
            "events required to close an observed committed lifecycle gap"
        ),
    }
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return {
        "injected_calls": inserted,
        "exactified_mint_drop_calls": exactified_calls,
        "exactified_mint_drop_tokens": exactified_tokens,
        "mint_event_transactions": len(all_events_by_tx),
        "supplemental_post_exactification_mint_events": len(supplemental_required),
        "overlap_instances": sorted(overlap),
    }


def write_report(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    tmp.replace(path)
    txt = path.with_suffix(".txt")
    lines = [
        "Vegeta S1 NFT lifecycle public-mint recovery",
        "",
        f"status: {data['status']}",
        f"blocks: {data['blocks']}",
        f"transactions: {data['transactions']}",
        f"initial missing token lifecycles: {data['initial_missing_tokens']}",
        f"affected instances: {len(data['affected_instances'])}",
        f"public mint events recovered: {data['public_mint_events_recovered']}",
        f"injected native mint effects: {data['injected_calls']}",
        f"exactified translated mint_drop calls: {data.get('exactified_mint_drop_calls', 0)}",
        f"exactified translated mint_drop tokens: {data.get('exactified_mint_drop_tokens', 0)}",
        f"remaining committed lifecycle gaps: {data['remaining_missing_tokens']}",
        "",
        "Method: public ERC721 Transfer(from=0) logs only; no debug_trace/SLOAD/SSTORE/storage-key oracle.",
        "Recovered calls are inserted into the exact committed source transaction that emitted each mint.",
    ]
    txt.write_text("\n".join(lines) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--execution-plan", type=Path, required=True)
    ap.add_argument("--manifest", type=Path, required=True)
    ap.add_argument("--report", type=Path, required=True)
    ap.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    ap.add_argument("--chunk-blocks", type=int, default=int(os.environ.get("VEGETA_S1_LIFECYCLE_LOG_CHUNK_BLOCKS", "250")))
    ap.add_argument(
        "--reconcile-instance",
        action="append",
        default=[],
        help=(
            "also reconcile every committed cw721-drop mint_drop on this instance against public "
            "Transfer(from=0) logs, even when the existence-only lifecycle scan has no gap; repeatable"
        ),
    )
    ns = ap.parse_args()
    if ns.chunk_blocks <= 0:
        raise SystemExit("--chunk-blocks must be positive")
    if not ns.execution_plan.is_file() or not ns.manifest.is_file():
        raise SystemExit("missing execution plan/manifest")

    manifest = json.loads(ns.manifest.read_text(encoding="utf-8"))
    scan = scan_plan(ns.execution_plan, manifest)
    if scan["blocks"] != int(manifest.get("blocks", -1)) or scan["transactions"] != int(manifest.get("transactions", -1)):
        raise SystemExit("execution plan domain disagrees with manifest")

    forced_instances = {str(value) for value in ns.reconcile_instance if str(value)}
    if forced_instances:
        known_families = {
            str(row.get("instance_id") or ""): str(row.get("family") or "")
            for row in manifest.get("instances") or []
        }
        unknown = sorted(iid for iid in forced_instances if iid not in known_families)
        wrong_family = sorted(
            iid for iid in forced_instances if known_families.get(iid) not in {None, "cw721-drop"}
        )
        no_translated_mints = sorted(
            iid for iid in forced_instances if iid not in set(scan["translated_mint_instances"])
        )
        if unknown:
            raise SystemExit(f"--reconcile-instance not present in manifest: {unknown}")
        if wrong_family:
            raise SystemExit(f"--reconcile-instance is not cw721-drop: {wrong_family}")
        if no_translated_mints:
            raise SystemExit(
                "--reconcile-instance has no committed translated mint_drop calls to exactify: "
                f"{no_translated_mints}"
            )
        scan["affected_instances"] = sorted(set(scan["affected_instances"]) | forced_instances)

    if not scan["gaps"] and not forced_instances:
        data = {
            "schema_version": 1,
            "status": "already-complete",
            "blocks": scan["blocks"],
            "transactions": scan["transactions"],
            "initial_missing_tokens": 0,
            "affected_instances": [],
            "public_mint_events_recovered": 0,
            "injected_calls": 0,
            "exactified_mint_drop_calls": 0,
            "exactified_mint_drop_tokens": 0,
            "remaining_missing_tokens": 0,
        }
        write_report(ns.report, data)
        print("PASS: full-domain committed NFT lifecycle already complete; no RPC/log recovery needed")
        return 0

    if not ns.rpc_url:
        sample = list(scan["gaps"].values())[:5]
        reason = (
            "committed NFT lifecycle gaps require" if scan["gaps"]
            else "explicit cw721-drop mint reconciliation requires"
        )
        raise SystemExit(
            f"{reason} one cheap eth_getLogs recovery pass, but ETH_RPC_URL is unset. "
            f"affected_instances={scan['affected_instances']} sample={json.dumps(sample, sort_keys=True)}"
        )

    iid_by_address = {iid_address(iid): iid for iid in scan["affected_instances"]}
    print(
        f"recovering S1 NFT lifecycle via public mint logs: gaps={len(scan['gaps'])} "
        f"instances={len(iid_by_address)} forced_reconcile_instances={len(forced_instances)} "
        f"blocks={scan['first_block']}..{scan['last_block']}",
        flush=True,
    )
    client = RpcClient(ns.rpc_url, timeout=60, retries=5, backoff=1.0)
    raw_logs = fetch_mint_logs(client, sorted(iid_by_address), int(scan["first_block"]), int(scan["last_block"]), ns.chunk_blocks)
    events = parse_logs(raw_logs, iid_by_address)
    required = validate_recovery(scan, events)
    overlap = sorted(set(scan["affected_instances"]) & set(scan["translated_mint_instances"]))
    print(
        f"public mint log recovery validated: events={len(events)} required_gap_events={len(required)} "
        f"translated_mint_overlap_instances={len(overlap)}",
        flush=True,
    )

    rewrite = rewrite_plan(ns.execution_plan, ns.manifest, manifest, scan, events, required)
    after = scan_plan(ns.execution_plan, json.loads(ns.manifest.read_text(encoding="utf-8")))

    data = {
        "schema_version": 1,
        "status": "repaired-and-validated",
        "blocks": scan["blocks"],
        "transactions": scan["transactions"],
        "initial_missing_tokens": len(scan["gaps"]),
        "affected_instances": scan["affected_instances"],
        "forced_reconcile_instances": sorted(forced_instances),
        "public_mint_events_recovered": len(events),
        "mint_event_transactions": rewrite["mint_event_transactions"],
        "injected_calls": rewrite["injected_calls"],
        "exactified_mint_drop_calls": rewrite["exactified_mint_drop_calls"],
        "exactified_mint_drop_tokens": rewrite["exactified_mint_drop_tokens"],
        "supplemental_post_exactification_mint_events": rewrite["supplemental_post_exactification_mint_events"],
        "translated_mint_overlap_instances": rewrite["overlap_instances"],
        "remaining_missing_tokens": 0,
        "first_missing_examples": list(scan["gaps"].values())[:20],
        "provenance": {
            "source": "public ERC721 Transfer(address,address,uint256) logs with indexed from=address(0)",
            "debug_trace_used": False,
            "concrete_storage_keys_used": False,
            "exact_event_token_ids_and_recipients": True,
        },
    }
    write_report(ns.report, data)
    print(
        f"PASS: repaired/reconciled full-domain committed NFT lifecycle from public mint logs: "
        f"instances={len(scan['affected_instances'])} forced={len(forced_instances)} events={len(events)} "
        f"exactified_calls={rewrite['exactified_mint_drop_calls']} injected={rewrite['injected_calls']} "
        f"supplemental={rewrite['supplemental_post_exactification_mint_events']} gaps=0",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
