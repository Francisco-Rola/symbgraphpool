#!/usr/bin/env python3
"""Audit cw721-drop translated mint quantities against the frozen public Transfer-log sequence.

This is a diagnostic only.  It never changes native semantics.  It replays the exact cw721-drop
mint adapter selection used by prepare-native-s3-execution.py, then groups missing, mismatched, and
unexpected committed mint translations by storage owner, selector, and reviewed entrypoint.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PLAN = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-plan/native-plan.jsonl"
DEFAULT_SELECTOR = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-plan/selector-semantic-map.json"
DEFAULT_CODE_CACHE = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-characterization/code-cache.json"
DEFAULT_SEQUENCE = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-characterization/cw721-drop-mint-sequence.json"
DEFAULT_JSON = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-plan/cw721-drop-translation-audit.json"
DEFAULT_TEXT = ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-plan/cw721-drop-translation-audit.txt"


def load_prepare_module():
    path = Path(__file__).with_name("prepare-native-s3-execution.py")
    spec = importlib.util.spec_from_file_location("vegeta_prepare_native_execution", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import preparation module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read_json(path: Path) -> Any:
    return json.loads(path.read_text())


def mint_expected(sequence: dict) -> dict[tuple[str, str], dict]:
    out: dict[tuple[str, str], dict] = {}
    for raw_owner, row in (sequence.get("owners") or {}).items():
        owner = str(raw_owner).lower()
        for raw_hash, txrow in (row.get("transactions") or {}).items():
            out[(owner, str(raw_hash).lower())] = txrow
    return out


def action_summary(action: dict, family: str | None = None, entrypoint: str | None = None) -> dict:
    args = action.get("arguments") or {}
    compact_args = {}
    for name in ("quantity", "nonce", "recipient", "trunk_id", "critter_id", "phase_index", "public_quantity"):
        if name in args:
            compact_args[name] = args[name]
    return {
        "action_id": action.get("action_id"),
        "call_type": action.get("call_type"),
        "selector": str(action.get("selector") or "0x").lower(),
        "entrypoint": entrypoint if entrypoint is not None else action.get("native_entrypoint"),
        "family": family if family is not None else action.get("native_code_family"),
        "translation_status": action.get("translation_status"),
        "failed_frame": bool(action.get("failed_frame")),
        "ethereum_code_address": action.get("ethereum_code_address"),
        "storage_context_address": action.get("storage_context_address"),
        "arguments": compact_args,
        "calldata_bytes": max(0, (len(str(action.get("ethereum_input") or "0x")) - 2) // 2),
    }


def is_drop_mint_entrypoint(entrypoint: str) -> bool:
    """Return True for reviewed cw721-drop entrypoints whose committed effect is token minting."""
    compact = str(entrypoint or "").lower().replace("_", "")
    return any(token in compact for token in ("mint", "purchase", "airdrop", "reservedrop"))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--plan", type=Path, default=DEFAULT_PLAN)
    ap.add_argument("--selector-map", type=Path, default=DEFAULT_SELECTOR)
    ap.add_argument("--code-cache", type=Path, default=DEFAULT_CODE_CACHE)
    ap.add_argument("--mint-sequence", type=Path, default=DEFAULT_SEQUENCE)
    ap.add_argument("--output", type=Path, default=DEFAULT_JSON)
    ap.add_argument("--text-output", type=Path, default=DEFAULT_TEXT)
    ns = ap.parse_args()

    mod = load_prepare_module()
    sequence = read_json(ns.mint_sequence)
    if sequence.get("dataset") != "vegeta-s1":
        raise SystemExit(f"unexpected mint-sequence dataset: {sequence.get('dataset')!r}")
    expected = mint_expected(sequence)
    reviewed_owners = {owner for owner, _ in expected}
    reviewed_owners.update(str(x).lower() for x in (sequence.get("reviewed_owners") or []))

    selector_doc = read_json(ns.selector_map)
    cache = {
        str(k).lower(): v
        for k, v in read_json(ns.code_cache).items()
        if isinstance(v, dict)
    }
    idx, fam_by_addr = mod.rule_index(selector_doc, cache)
    token_ids = mod.TokenIdRemapper()
    sequence_model = mod.Cw721DropMintSequence(ns.mint_sequence)

    translated: dict[tuple[str, str], int] = defaultdict(int)
    attempts: dict[tuple[str, str], list[dict]] = defaultdict(list)
    candidates: dict[tuple[str, str], list[dict]] = defaultdict(list)
    tx_meta: dict[tuple[str, str], dict] = {}
    counters = Counter()

    with ns.plan.open(encoding="utf-8") as handle:
        for line in handle:
            if not line.strip():
                continue
            block = json.loads(line)
            bn = int(block.get("block_number", 0))
            for tx in block.get("transactions") or []:
                tx_hash = str(tx.get("tx_hash") or "").lower()
                by_id = {
                    int(a["action_id"]): a
                    for a in (tx.get("native_actions") or [])
                    if a.get("action_id") is not None
                }
                for action in tx.get("native_actions") or []:
                    owner = mod.norm_addr(action.get("storage_context_address"))
                    if owner not in reviewed_owners:
                        continue
                    if str(action.get("native_code_family") or "") != "cw721-drop":
                        continue
                    # Only source-committed effects can be compared with committed Transfer logs.
                    if bool(tx.get("source_failed")) or mod.source_revert_scope_action_id(action, by_id) is not None:
                        continue

                    key = (owner, tx_hash)
                    tx_meta.setdefault(key, {"block_number": bn, "source_failed": bool(tx.get("source_failed"))})

                    fam = action.get("native_code_family")
                    ep = action.get("native_entrypoint")
                    sig = None
                    if not fam or not ep or str(ep).startswith("opaque::"):
                        rule = mod.match_rule(action, idx, fam_by_addr)
                        if rule:
                            fam = rule.get("native_code_family")
                            ep = rule.get("native_entrypoint")
                            sig = rule.get("ethereum_function_signature")

                    # Keep top-level/direct storage-context calls around to diagnose source mint
                    # events for which no currently-reviewed mint adapter exists.
                    if str(action.get("translation_status")) != "inlined-delegatecall":
                        candidates[key].append(action_summary(action, str(fam) if fam else None, str(ep) if ep else None))

                    if str(fam or "") != "cw721-drop" or not ep:
                        continue
                    if not is_drop_mint_entrypoint(str(ep)):
                        continue

                    row = action_summary(action, str(fam), str(ep))
                    try:
                        caller = mod.caller_for(tx, action, by_id, "exact")
                        call = mod.translate(str(fam), str(ep), sig, tx, action, caller, token_ids, None, sequence_model)
                        if call is None:
                            row["adapter_result"] = "none"
                            counters["adapter_none"] += 1
                        elif call.get("kind") == "execute" and "mint_drop" in (call.get("msg") or {}):
                            quantity = int(call["msg"]["mint_drop"]["quantity"])
                            row["adapter_result"] = "mint_drop"
                            row["translated_quantity"] = quantity
                            translated[key] += quantity
                            counters["translated_mint_actions"] += 1
                        else:
                            row["adapter_result"] = str(call.get("kind") or "other")
                    except Exception as exc:  # diagnostic must report adapter errors rather than aborting early
                        row["adapter_result"] = "error"
                        row["error"] = f"{type(exc).__name__}: {exc}"
                        counters["adapter_errors"] += 1
                    attempts[key].append(row)

    issues = []
    exact = 0
    keys = sorted(set(expected) | set(translated))
    for key in keys:
        exp = expected.get(key)
        wanted = int((exp or {}).get("mint_count", 0))
        actual = int(translated.get(key, 0))
        if exp is not None and actual == wanted:
            exact += 1
            continue
        if exp is None:
            kind = "unexpected-translated-mint"
        elif actual == 0:
            kind = "missing-translated-mint"
        else:
            kind = "quantity-mismatch"
        issues.append({
            "kind": kind,
            "owner": key[0],
            "tx_hash": key[1],
            "block_number": (tx_meta.get(key) or {}).get("block_number", (exp or {}).get("block_number")),
            "expected_mint_events": wanted,
            "translated_quantity": actual,
            "token_ids": (exp or {}).get("token_ids") or [],
            "mint_adapter_attempts": attempts.get(key) or [],
            "committed_cw721_drop_candidates": candidates.get(key) or [],
        })

    groups: dict[tuple[str, str, str], dict] = {}
    for issue in issues:
        rows = issue["mint_adapter_attempts"]
        if not rows:
            rows = issue["committed_cw721_drop_candidates"] or [{"selector": "<none>", "entrypoint": "<none>"}]
        # Attribute once per selector/entrypoint in the transaction. This is diagnostic grouping, not
        # an additive count, so a tx with multiple candidates can appear in multiple groups.
        seen = set()
        for row in rows:
            gkey = (issue["owner"], str(row.get("selector") or "<none>"), str(row.get("entrypoint") or "<none>"))
            if gkey in seen:
                continue
            seen.add(gkey)
            group = groups.setdefault(gkey, {
                "owner": gkey[0], "selector": gkey[1], "entrypoint": gkey[2],
                "issue_transactions": 0, "missing_transactions": 0, "mismatch_transactions": 0,
                "unexpected_transactions": 0, "expected_mint_events": 0, "translated_quantity": 0,
                "adapter_none_transactions": 0, "samples": [],
            })
            group["issue_transactions"] += 1
            if issue["kind"] == "missing-translated-mint": group["missing_transactions"] += 1
            elif issue["kind"] == "quantity-mismatch": group["mismatch_transactions"] += 1
            else: group["unexpected_transactions"] += 1
            group["expected_mint_events"] += int(issue["expected_mint_events"])
            group["translated_quantity"] += int(issue["translated_quantity"])
            if any(x.get("adapter_result") == "none" for x in issue["mint_adapter_attempts"]):
                group["adapter_none_transactions"] += 1
            if len(group["samples"]) < 5:
                group["samples"].append({
                    "tx_hash": issue["tx_hash"], "kind": issue["kind"],
                    "expected_mint_events": issue["expected_mint_events"],
                    "translated_quantity": issue["translated_quantity"],
                    "token_ids": issue["token_ids"][:12],
                })

    grouped = sorted(groups.values(), key=lambda x: (-x["issue_transactions"], -x["unexpected_transactions"], x["owner"], x["selector"]))
    kind_counts = Counter(issue["kind"] for issue in issues)
    report = {
        "schema_version": 1,
        "dataset": "vegeta-s1",
        "definition": "local correlation of current cw721-drop native mint adapters with frozen public zero-address Transfer mint events; diagnostic only",
        "inputs": {"plan": str(ns.plan), "selector_map": str(ns.selector_map), "code_cache": str(ns.code_cache), "mint_sequence": str(ns.mint_sequence)},
        "summary": {
            "expected_mint_transactions": len(expected),
            "translated_mint_transactions": len(translated),
            "exact_transactions": exact,
            "issue_transactions": len(issues),
            "missing_translated_mint_transactions": kind_counts["missing-translated-mint"],
            "quantity_mismatch_transactions": kind_counts["quantity-mismatch"],
            "unexpected_translated_mint_transactions": kind_counts["unexpected-translated-mint"],
            "expected_mint_events": sum(int(row.get("mint_count", 0)) for row in expected.values()),
            "translated_mint_quantity": sum(translated.values()),
            **dict(counters),
        },
        "groups": grouped,
        "issues": issues,
        "semantics_changed": False,
        "concrete_storage_keys_used": False,
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    lines = [
        "Vegeta S1 cw721-drop mint translation audit",
        "",
        f"expected mint transactions: {len(expected)}",
        f"translated mint transactions: {len(translated)}",
        f"exact transactions: {exact}",
        f"issue transactions: {len(issues)}",
        f"  missing translated mint: {kind_counts['missing-translated-mint']}",
        f"  quantity mismatch: {kind_counts['quantity-mismatch']}",
        f"  unexpected translated mint: {kind_counts['unexpected-translated-mint']}",
        f"expected mint events: {sum(int(row.get('mint_count', 0)) for row in expected.values())}",
        f"translated mint quantity: {sum(translated.values())}",
        "",
        "Top issue groups (transactions may appear in more than one group when a tx has multiple candidate calls):",
    ]
    for group in grouped[:40]:
        lines.append(
            "  {owner} {selector} entrypoint={entrypoint} issues={issue_transactions} "
            "missing={missing_transactions} mismatch={mismatch_transactions} unexpected={unexpected_transactions} "
            "expected_events={expected_mint_events} translated_qty={translated_quantity} adapter_none={adapter_none_transactions}".format(**group)
        )
        for sample in group["samples"][:3]:
            lines.append(
                f"    {sample['kind']} tx={sample['tx_hash']} expected={sample['expected_mint_events']} "
                f"translated={sample['translated_quantity']} token_ids={sample['token_ids']}"
            )
    lines.extend([
        "",
        "Interpretation:",
        "  Do not override quantities from Transfer logs merely to make preparation pass.",
        "  Use this grouping to tighten owner/selector semantics or add an independently reviewed",
        "  event-backed adapter where calldata does not determine the committed mint cardinality.",
        "  This audit changes no native-plan or execution semantics.",
    ])
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:14]))
    print(f"json: {ns.output}")
    print(f"text: {ns.text_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
