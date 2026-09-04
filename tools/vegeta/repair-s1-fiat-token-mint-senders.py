#!/usr/bin/env python3
"""Repair committed S1 FiatToken mint calls to use the native synthetic admin capability.

The S1 execution adapter models source-successful FiatToken/USDC mint effects with the
`fiat-token-cw20` contract.  That native contract intentionally has one synthetic admin for
controlled supply changes; it does not reconstruct Circle's historical minter-role registry.
Older prepared plans preserved the Ethereum minter as the CosmWasm sender, so an otherwise valid
source mint deterministically fails the native `ensure_admin` check.

This repair is deliberately narrow:
* only committed `fiat-token-cw20` `mint` execute calls are touched;
* the original Ethereum sender is retained in `source_minter` metadata;
* the message, recipient, amount, instance, transaction ordering, and call ordering are unchanged;
* source-failed transactions and caught internal-revert scopes are left untouched;
* the plan is replaced atomically only after a full verification scan succeeds.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

NATIVE_ADMIN = "native-s3-admin"
ADAPTER = "source-successful-fiat-token-mint-via-native-admin"
FAMILY = "fiat-token-cw20"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def is_committed_mint(tx: dict[str, Any], call: dict[str, Any]) -> bool:
    if bool(tx.get("source_failed")):
        return False
    if call.get("source_revert_scope_action_id") is not None:
        return False
    if str(call.get("family") or "") != FAMILY:
        return False
    return isinstance((call.get("msg") or {}).get("mint"), dict)


def scan(path: Path) -> dict[str, Any]:
    blocks = txs = calls = mints = adapted = mismatched = 0
    mismatches: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            block = json.loads(line)
            blocks += 1
            bn = int(block["block_number"])
            for ti, tx in enumerate(block.get("transactions") or []):
                txs += 1
                for ci, call in enumerate(tx.get("calls") or []):
                    calls += 1
                    if not is_committed_mint(tx, call):
                        continue
                    mints += 1
                    sender = str(call.get("sender") or "")
                    if sender == NATIVE_ADMIN:
                        adapted += 1
                    else:
                        mismatched += 1
                        if len(mismatches) < 20:
                            mismatches.append({
                                "block_number": bn,
                                "transaction_index": int(tx.get("transaction_index", ti)),
                                "call_index": ci,
                                "tx_hash": str(tx.get("tx_hash") or ""),
                                "instance_id": str(call.get("instance_id") or ""),
                                "sender": sender,
                                "recipient": str(((call.get("msg") or {}).get("mint") or {}).get("recipient") or ""),
                                "amount": str(((call.get("msg") or {}).get("mint") or {}).get("amount") or ""),
                            })
    return {
        "blocks": blocks,
        "transactions": txs,
        "calls": calls,
        "committed_fiat_token_mints": mints,
        "adapted_mints": adapted,
        "mismatched_mints": mismatched,
        "mismatch_sample": mismatches,
    }


def rewrite(path: Path) -> dict[str, Any]:
    before_hash = sha256(path)
    tmp = path.with_suffix(path.suffix + ".fiat-mint-admin.tmp")
    changed = 0
    source_minters: set[str] = set()
    touched_instances: set[str] = set()
    try:
        with path.open(encoding="utf-8") as src, tmp.open("w", encoding="utf-8") as dst:
            for line in src:
                if not line.strip():
                    continue
                block = json.loads(line)
                for tx in block.get("transactions") or []:
                    for call in tx.get("calls") or []:
                        if not is_committed_mint(tx, call):
                            continue
                        sender = str(call.get("sender") or "")
                        if sender == NATIVE_ADMIN:
                            # Plans produced by the corrected translator already contain provenance.
                            if call.get("source_authorization_adapter") == ADAPTER and call.get("source_minter"):
                                source_minters.add(str(call["source_minter"]))
                            continue
                        if not sender:
                            raise RuntimeError(
                                "committed fiat-token-cw20 mint is missing its source sender: "
                                f"tx={tx.get('tx_hash')} instance={call.get('instance_id')}"
                            )
                        existing = call.get("source_minter")
                        if existing is not None and str(existing) != sender:
                            raise RuntimeError(
                                "existing source_minter metadata disagrees with mint sender: "
                                f"tx={tx.get('tx_hash')} sender={sender} source_minter={existing}"
                            )
                        call["source_minter"] = sender
                        call["source_authorization_adapter"] = ADAPTER
                        call["sender"] = NATIVE_ADMIN
                        source_minters.add(sender)
                        touched_instances.add(str(call.get("instance_id") or ""))
                        changed += 1
                dst.write(json.dumps(block, sort_keys=True, separators=(",", ":")) + "\n")

        verification = scan(tmp)
        if verification["mismatched_mints"]:
            raise RuntimeError(
                "fiat-token mint sender repair verification failed: "
                f"remaining={verification['mismatched_mints']} sample={verification['mismatch_sample'][:3]}"
            )
        if changed:
            tmp.replace(path)
        else:
            tmp.unlink(missing_ok=True)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise

    after_hash = sha256(path)
    return {
        "status": "repaired-and-validated" if changed else "already-complete",
        "changed_mints": changed,
        "source_minters_touched": len(source_minters),
        "instances_touched": len({x for x in touched_instances if x}),
        "plan_sha256_before": before_hash,
        "plan_sha256_after": after_hash,
        **scan(path),
    }


def write_report(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--execution-plan", type=Path, required=True)
    ap.add_argument("--report", type=Path)
    ns = ap.parse_args()
    if not ns.execution_plan.is_file():
        raise SystemExit(f"missing execution plan: {ns.execution_plan}")

    before = scan(ns.execution_plan)
    print(
        "preflighting S1 FiatToken mint authorization adapter: "
        f"mints={before['committed_fiat_token_mints']} mismatched_senders={before['mismatched_mints']}",
        flush=True,
    )
    result = rewrite(ns.execution_plan)
    if ns.report:
        write_report(ns.report, result)
    print(
        "PASS: S1 FiatToken committed mint senders are native-admin executable: "
        f"mints={result['committed_fiat_token_mints']} changed={result['changed_mints']} "
        f"remaining={result['mismatched_mints']}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
