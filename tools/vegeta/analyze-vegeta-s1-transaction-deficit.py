#!/usr/bin/env python3
"""Explain the remaining Vegeta S1 reviewed-state transaction deficit.

The publication-style S1 gate intentionally counts a source transaction as semantic only when its
translated call tree contains at least one *successful* reviewed state-dependent action.  That
all-transactions denominator is deliberately conservative, but it can hide whether the remaining
shortfall is concentrated in genuinely state-bearing/conflict-bearing source transactions or in
background traffic that is irrelevant to scheduler contention.

This streaming diagnostic keeps the frozen gate unchanged and reports three views side by side:

* all retained source transactions (the current publication gate denominator),
* source transactions with at least one concrete public-RPC storage access, and
* source transactions that participate in at least one source conflict pair.

It also ranks mapped-owner opaque selectors by unique currently-deficit transactions rather than by
raw call-frame count, and separately ranks unmapped/background fallback call targets.  The report is
for review prioritization only; no historical storage keys are copied into the native plan.
"""
from __future__ import annotations

import argparse
import json
import math
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from native_s3_planner_compat import (
    SEMANTIC_OPAQUE,
    SEMANTIC_PURE,
    SEMANTIC_READ_WRITE,
    SEMANTIC_STATE_READ,
    SEMANTIC_STATE_WRITE,
    block_conflicts_by_owner,
    semantic_effect_for_entrypoint,
)
from vegeta_corpus import iter_blocks

STATE_EFFECTS = {SEMANTIC_STATE_READ, SEMANTIC_STATE_WRITE, SEMANTIC_READ_WRITE}
Candidate = tuple[str, str, str]  # storage owner, native family, selector
FallbackCandidate = tuple[str, str, str]  # code address, selector, call type
TxId = int  # streaming transaction ordinal; compact identity for per-candidate uniqueness


def action_effect(action: dict[str, Any]) -> str:
    effect = str(action.get("semantic_effect") or "")
    if effect in STATE_EFFECTS | {SEMANTIC_PURE, SEMANTIC_OPAQUE}:
        return effect
    derived = semantic_effect_for_entrypoint(
        action.get("semantic_entrypoint") or action.get("native_entrypoint"),
        str(action.get("dispatch") or ""),
    )
    if derived != SEMANTIC_OPAQUE:
        return derived
    # Compatibility with older fixture/generated plans that predate semantic_effect.
    dispatch = str(action.get("dispatch") or "")
    if dispatch in {"mapped-entrypoint", "inlined-reviewed-entrypoint"}:
        return SEMANTIC_READ_WRITE
    if dispatch == "reviewed-stateless-entrypoint":
        return SEMANTIC_PURE
    return SEMANTIC_OPAQUE


def successful_reviewed_state(action: dict[str, Any]) -> bool:
    return not bool(action.get("failed_frame")) and action_effect(action) in STATE_EFFECTS


def reviewed_state_touch(action: dict[str, Any]) -> bool:
    return action_effect(action) in STATE_EFFECTS


def candidate_frame_class(action: dict[str, Any]) -> str:
    if bool(action.get("failed_frame")):
        return "reverted"
    if str(action.get("call_type") or "").upper() == "STATICCALL":
        return "successful-read-only"
    return "successful-state-capable"


def ratio(num: int, den: int) -> float:
    return num / den if den else 1.0


def denominator_row(
    name: str,
    transactions: int,
    successful_count: int,
    touch_count: int,
    source_gas_used: int,
    successful_gas_used: int,
    touch_gas_used: int,
) -> dict[str, Any]:
    return {
        "name": name,
        "transactions": transactions,
        "successful_reviewed_state_transactions": successful_count,
        "successful_reviewed_state_coverage": ratio(successful_count, transactions),
        "reviewed_state_touch_transactions": touch_count,
        "reviewed_state_touch_coverage": ratio(touch_count, transactions),
        "successful_state_deficit_transactions": transactions - successful_count,
        "source_gas_used": source_gas_used,
        "successful_reviewed_state_gas_used": successful_gas_used,
        "successful_reviewed_state_gas_coverage": ratio(successful_gas_used, source_gas_used),
        "reviewed_state_touch_gas_used": touch_gas_used,
        "reviewed_state_touch_gas_coverage": ratio(touch_gas_used, source_gas_used),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--native-plan", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--target-coverage", type=float, default=0.80)
    ap.add_argument("--conflict-target-coverage", type=float, default=0.80)
    ap.add_argument("--top", type=int, default=100)
    ap.add_argument("--dataset", default="vegeta-s1")
    ns = ap.parse_args()
    if not (0.0 <= ns.target_coverage <= 1.0):
        raise SystemExit("--target-coverage must be between 0 and 1")
    if not (0.0 <= ns.conflict_target_coverage <= 1.0):
        raise SystemExit("--conflict-target-coverage must be between 0 and 1")

    all_tx_count = successful_tx_count = state_touch_tx_count = 0
    source_state_tx_count = source_state_successful_count = source_state_touch_count = 0
    conflict_tx_count = conflict_successful_count = conflict_touch_count = 0
    all_gas = successful_gas = state_touch_gas = 0
    source_state_gas = source_state_successful_gas = source_state_touch_gas = 0
    conflict_gas = conflict_successful_gas = conflict_touch_gas = 0
    pure_only_tx_count = 0

    mapped_opaque_txs: dict[Candidate, set[TxId]] = defaultdict(set)
    mapped_opaque_conflict_txs: dict[Candidate, set[TxId]] = defaultdict(set)
    mapped_opaque_state_txs: dict[Candidate, set[TxId]] = defaultdict(set)
    mapped_opaque_frames: Counter[Candidate] = Counter()
    mapped_opaque_classes: Counter[tuple[Candidate, str]] = Counter()
    mapped_opaque_samples: dict[Candidate, list[str]] = defaultdict(list)

    fallback_txs: dict[FallbackCandidate, set[TxId]] = defaultdict(set)
    fallback_conflict_txs: dict[FallbackCandidate, set[TxId]] = defaultdict(set)
    fallback_state_txs: dict[FallbackCandidate, set[TxId]] = defaultdict(set)
    fallback_frames: Counter[FallbackCandidate] = Counter()
    fallback_samples: dict[FallbackCandidate, list[str]] = defaultdict(list)

    deficit_reason_counts = Counter()
    block_count = 0
    plan_handle = ns.native_plan.open(encoding="utf-8")
    try:
        for source_block in iter_blocks(ns.corpus):
            line = next((x for x in plan_handle if x.strip()), None)
            if line is None:
                raise SystemExit(f"native plan ended before source block {source_block['block_number']}")
            plan_block = json.loads(line)
            bn = int(source_block["block_number"])
            if int(plan_block.get("block_number", -1)) != bn:
                raise SystemExit(f"block mismatch source={bn} plan={plan_block.get('block_number')}")
            source_txs = source_block.get("transactions") or []
            plan_txs = plan_block.get("transactions") or []
            if len(source_txs) != len(plan_txs):
                raise SystemExit(f"block {bn}: source tx={len(source_txs)} plan tx={len(plan_txs)}")

            all_pairs, _ = block_conflicts_by_owner(source_block)
            conflict_indices = {idx for pair in all_pairs for idx in pair}

            for position, (source_tx, plan_tx) in enumerate(zip(source_txs, plan_txs)):
                tid = all_tx_count
                all_tx_count += 1
                tx_gas = int(source_tx.get("gas_used", 0) or 0)
                all_gas += tx_gas
                has_source_state = bool(source_tx.get("reads") or source_tx.get("writes"))
                is_conflict_participant = position in conflict_indices
                if has_source_state:
                    source_state_tx_count += 1
                    source_state_gas += tx_gas
                if is_conflict_participant:
                    conflict_tx_count += 1
                    conflict_gas += tx_gas

                actions = plan_tx.get("native_actions") or []
                has_successful_state = any(successful_reviewed_state(a) for a in actions)
                has_state_touch = any(reviewed_state_touch(a) for a in actions)
                has_successful_pure = any(
                    not bool(a.get("failed_frame")) and action_effect(a) == SEMANTIC_PURE for a in actions
                )
                if has_successful_state:
                    successful_tx_count += 1
                    successful_gas += tx_gas
                    if has_source_state:
                        source_state_successful_count += 1
                        source_state_successful_gas += tx_gas
                    if is_conflict_participant:
                        conflict_successful_count += 1
                        conflict_successful_gas += tx_gas
                if has_state_touch:
                    state_touch_tx_count += 1
                    state_touch_gas += tx_gas
                    if has_source_state:
                        source_state_touch_count += 1
                        source_state_touch_gas += tx_gas
                    if is_conflict_participant:
                        conflict_touch_count += 1
                        conflict_touch_gas += tx_gas
                if has_successful_pure and not has_state_touch:
                    pure_only_tx_count += 1
                if has_successful_state:
                    continue

                tx_hash = str(source_tx.get("tx_hash") or plan_tx.get("tx_hash") or "").lower()
                saw_mapped_opaque = False
                saw_fallback = False
                for action in actions:
                    dispatch = str(action.get("dispatch") or "")
                    status = str(action.get("translation_status") or "")
                    selector = str(action.get("selector") or "0x").lower()
                    if dispatch == "mapped-opaque-selector":
                        owner = str(action.get("storage_context_address") or "").lower()
                        family = str(action.get("native_code_family") or "")
                        if owner.startswith("0x") and len(owner) == 42 and family:
                            cand: Candidate = (owner, family, selector)
                            mapped_opaque_txs[cand].add(tid)
                            if is_conflict_participant:
                                mapped_opaque_conflict_txs[cand].add(tid)
                            if has_source_state:
                                mapped_opaque_state_txs[cand].add(tid)
                            mapped_opaque_frames[cand] += 1
                            mapped_opaque_classes[(cand, candidate_frame_class(action))] += 1
                            if tx_hash and tx_hash not in mapped_opaque_samples[cand] and len(mapped_opaque_samples[cand]) < 5:
                                mapped_opaque_samples[cand].append(tx_hash)
                            saw_mapped_opaque = True
                    if status == "background-fallback" or dispatch == "background-fallback":
                        code = str(action.get("ethereum_code_address") or "<unknown>").lower()
                        call_type = str(action.get("call_type") or "UNKNOWN").upper()
                        cand2: FallbackCandidate = (code, selector, call_type)
                        fallback_txs[cand2].add(tid)
                        if is_conflict_participant:
                            fallback_conflict_txs[cand2].add(tid)
                        if has_source_state:
                            fallback_state_txs[cand2].add(tid)
                        fallback_frames[cand2] += 1
                        if tx_hash and tx_hash not in fallback_samples[cand2] and len(fallback_samples[cand2]) < 5:
                            fallback_samples[cand2].append(tx_hash)
                        saw_fallback = True
                if saw_mapped_opaque:
                    deficit_reason_counts["has_mapped_owner_opaque_selector"] += 1
                if saw_fallback:
                    deficit_reason_counts["has_background_fallback"] += 1
                if not saw_mapped_opaque and not saw_fallback:
                    deficit_reason_counts["no_opaque_or_fallback_candidate"] += 1

            block_count += 1
            if block_count % 500 == 0:
                print(
                    f"S1 transaction-deficit blocks={block_count} tx={all_tx_count} "
                    f"successful={successful_tx_count} conflict_participants={conflict_tx_count}",
                    flush=True,
                )
        extra = next((x for x in plan_handle if x.strip()), None)
        if extra is not None:
            extra_block = json.loads(extra).get("block_number")
            raise SystemExit(f"native plan contains extra block after source corpus: {extra_block}")
    finally:
        plan_handle.close()

    if block_count == 0:
        raise SystemExit("source corpus is empty")

    all_row = denominator_row(
        "all-source-transactions", all_tx_count, successful_tx_count, state_touch_tx_count,
        all_gas, successful_gas, state_touch_gas,
    )
    state_row = denominator_row(
        "source-storage-access-transactions",
        source_state_tx_count,
        source_state_successful_count,
        source_state_touch_count,
        source_state_gas,
        source_state_successful_gas,
        source_state_touch_gas,
    )
    conflict_row = denominator_row(
        "source-conflict-participating-transactions",
        conflict_tx_count,
        conflict_successful_count,
        conflict_touch_count,
        conflict_gas,
        conflict_successful_gas,
        conflict_touch_gas,
    )

    target_success = math.ceil(ns.target_coverage * all_tx_count - 1e-12)
    additional_for_gate = max(0, target_success - successful_tx_count)
    conflict_target_success = math.ceil(ns.conflict_target_coverage * conflict_tx_count - 1e-12)
    additional_for_conflict_target = max(0, conflict_target_success - conflict_successful_count)

    opaque_rows = []
    for cand, txs in mapped_opaque_txs.items():
        owner, family, selector = cand
        cls = {
            name: mapped_opaque_classes[(cand, name)]
            for name in ("successful-state-capable", "successful-read-only", "reverted")
            if mapped_opaque_classes[(cand, name)]
        }
        opaque_rows.append({
            "storage_owner": owner,
            "native_code_family": family,
            "selector": selector,
            "deficit_transactions": len(txs),
            "conflict_participant_deficit_transactions": len(mapped_opaque_conflict_txs[cand]),
            "source_state_access_deficit_transactions": len(mapped_opaque_state_txs[cand]),
            "frames": mapped_opaque_frames[cand],
            "frame_classification": cls,
            "sample_tx_hashes": mapped_opaque_samples[cand],
        })
    opaque_rows.sort(key=lambda r: (
        -r["deficit_transactions"],
        -r["conflict_participant_deficit_transactions"],
        -r["source_state_access_deficit_transactions"],
        -r["frames"],
        r["storage_owner"],
        r["selector"],
    ))

    fallback_rows = []
    for cand, txs in fallback_txs.items():
        code, selector, call_type = cand
        fallback_rows.append({
            "ethereum_code_address": code,
            "selector": selector,
            "call_type": call_type,
            "deficit_transactions": len(txs),
            "conflict_participant_deficit_transactions": len(fallback_conflict_txs[cand]),
            "source_state_access_deficit_transactions": len(fallback_state_txs[cand]),
            "frames": fallback_frames[cand],
            "sample_tx_hashes": fallback_samples[cand],
        })
    fallback_rows.sort(key=lambda r: (
        -r["deficit_transactions"],
        -r["conflict_participant_deficit_transactions"],
        -r["source_state_access_deficit_transactions"],
        -r["frames"],
        r["ethereum_code_address"],
        r["selector"],
    ))

    report = {
        "schema_version": 1,
        "dataset": ns.dataset,
        "definition": {
            "successful_reviewed_state_transaction": "transaction contains at least one non-reverted STATE_READ/STATE_WRITE/READ_WRITE reviewed action",
            "current_publication_denominator": "all retained S1 source transactions",
            "alternative_denominators_are_diagnostic_only": True,
            "source_state_access_denominator": "transactions with at least one public-RPC source reads/writes storage record",
            "source_conflict_participant_denominator": "transactions that are an endpoint of at least one source conflict pair",
        },
        "blocks": block_count,
        "target_coverage": ns.target_coverage,
        "target_successful_reviewed_state_transactions": target_success,
        "additional_successful_reviewed_state_transactions_needed_for_current_gate": additional_for_gate,
        "contention_scheduler_diagnostic": {
            "target_coverage": ns.conflict_target_coverage,
            "target_successful_reviewed_state_transactions": conflict_target_success,
            "additional_successful_reviewed_state_transactions_needed": additional_for_conflict_target,
            "coverage": conflict_row["successful_reviewed_state_coverage"],
            "successful_reviewed_state_transactions": conflict_row["successful_reviewed_state_transactions"],
            "source_conflict_participating_transactions": conflict_row["transactions"],
            "target_met": additional_for_conflict_target == 0,
            "publication_gate_changed": False,
        },
        "denominators": {
            "all_source_transactions": all_row,
            "source_storage_access_transactions": state_row,
            "source_conflict_participating_transactions": conflict_row,
        },
        "reviewed_pure_only_transactions": pure_only_tx_count,
        "deficit_reason_counts": dict(deficit_reason_counts),
        "mapped_owner_opaque_candidates": opaque_rows[: max(0, ns.top)],
        "background_fallback_candidates": fallback_rows[: max(0, ns.top)],
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    lines = [
        f"Vegeta {ns.dataset} successful reviewed-state transaction deficit",
        "",
        f"current publication denominator (all tx): {all_row['successful_reviewed_state_transactions']}/{all_row['transactions']} ({100*all_row['successful_reviewed_state_coverage']:.2f}%)",
        f"target: {100*ns.target_coverage:.2f}% = {target_success} successful reviewed-state tx",
        f"additional successful reviewed-state tx needed: {additional_for_gate}",
        f"reviewed state-touch incl. reverted (all tx): {all_row['reviewed_state_touch_transactions']}/{all_row['transactions']} ({100*all_row['reviewed_state_touch_coverage']:.2f}%)",
        f"reviewed pure-only transactions: {pure_only_tx_count}",
        "",
        "Diagnostic denominator alignment (does not change the frozen gate):",
        f"  source storage-access tx: {state_row['successful_reviewed_state_transactions']}/{state_row['transactions']} ({100*state_row['successful_reviewed_state_coverage']:.2f}%)",
        f"  source storage-access gas: {state_row['successful_reviewed_state_gas_used']}/{state_row['source_gas_used']} ({100*state_row['successful_reviewed_state_gas_coverage']:.2f}%)",
        f"  source conflict-participant tx: {conflict_row['successful_reviewed_state_transactions']}/{conflict_row['transactions']} ({100*conflict_row['successful_reviewed_state_coverage']:.2f}%)",
        f"  source conflict-participant gas: {conflict_row['successful_reviewed_state_gas_used']}/{conflict_row['source_gas_used']} ({100*conflict_row['successful_reviewed_state_gas_coverage']:.2f}%)",
        f"  contention-oriented 80% target: {conflict_target_success}/{conflict_row['transactions']} (need {additional_for_conflict_target} additional successful reviewed-state conflict participants; {'PASS' if additional_for_conflict_target == 0 else 'FAIL'})",
        "",
        "Top mapped-owner opaque selectors by current deficit transactions:",
    ]
    for row in opaque_rows[: min(max(0, ns.top), 30)]:
        cls = row["frame_classification"]
        lines.append(
            f"  {row['storage_owner']} {row['selector']} family={row['native_code_family']} "
            f"deficit_tx={row['deficit_transactions']} conflict_tx={row['conflict_participant_deficit_transactions']} "
            f"state_access_tx={row['source_state_access_deficit_transactions']} frames={row['frames']} "
            f"state={cls.get('successful-state-capable',0)} read={cls.get('successful-read-only',0)} reverted={cls.get('reverted',0)}"
        )
    lines += ["", "Top unmapped/background fallback targets by current deficit transactions:"]
    for row in fallback_rows[: min(max(0, ns.top), 30)]:
        lines.append(
            f"  {row['ethereum_code_address']} {row['selector']} type={row['call_type']} "
            f"deficit_tx={row['deficit_transactions']} conflict_tx={row['conflict_participant_deficit_transactions']} "
            f"state_access_tx={row['source_state_access_deficit_transactions']} frames={row['frames']}"
        )
    lines += [
        "",
        "Interpretation: use conflict_tx/state_access_tx to decide whether the 80% all-transaction gate",
        "is exposing missing benchmark semantics or mostly non-contention background. Do not lower the",
        "publication gate solely because a diagnostic denominator produces a higher percentage.",
        "The explicit contention target is a scheduler-fidelity diagnostic until the benchmark methodology",
        "formally adopts it; it does not replace the frozen all-transaction semantic-replay gate.",
    ]
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
