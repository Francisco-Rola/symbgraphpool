#!/usr/bin/env python3
"""Stream a native CosmWasm call plan for Vegeta S1 using the reviewed S3 family mechanisms.

This planner deliberately consumes ``thin-corpus.jsonl`` rather than the full public-RPC corpus, so
historical read/write sets cannot leak into Rust-ACG prediction.  The independent streaming source
coverage audit is joined only into the reporting metadata.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

from native_s3_planner_compat import (
    FamilyResolver, SEMANTIC_OPAQUE, SEMANTIC_PURE, SEMANTIC_READ_WRITE, SEMANTIC_STATE_READ,
    SEMANTIC_STATE_WRITE, SEMANTIC_TRANSLATION_STATUSES, SYSTEM_TRANSLATION_STATUS,
    implementation_readiness, load_code_cache, semantic_effect_for_entrypoint, translate_call_tree,
    validate_frozen_map,
)
from vegeta_corpus import iter_blocks

ROOT = Path(__file__).resolve().parents[2]

STATE_EFFECTS = {SEMANTIC_STATE_READ, SEMANTIC_STATE_WRITE, SEMANTIC_READ_WRITE}


def action_effect(action: dict) -> str:
    effect = str(action.get("semantic_effect") or "")
    if effect in STATE_EFFECTS | {SEMANTIC_PURE, SEMANTIC_OPAQUE}:
        return effect
    return semantic_effect_for_entrypoint(
        action.get("semantic_entrypoint") or action.get("native_entrypoint"),
        str(action.get("dispatch") or ""),
    )


def is_successful_reviewed_state_action(action: dict) -> bool:
    return not action.get("failed_frame") and action_effect(action) in STATE_EFFECTS


def is_reviewed_state_touch(action: dict) -> bool:
    return action_effect(action) in STATE_EFFECTS


def is_successful_reviewed_pure(action: dict) -> bool:
    return not action.get("failed_frame") and action_effect(action) == SEMANTIC_PURE


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def render(report: dict) -> str:
    source = report["source_conflict_coverage"]
    storage = report["storage_access_coverage"]
    calls = report["calls"]
    txc = report["transaction_semantic_coverage"]
    return "\n".join([
        "Vegeta S1 native translation pre-execution coverage",
        "",
        f"blocks retained: {report['blocks_retained']} / {report['source_blocks']}",
        f"transactions retained: {report['transactions_retained']} / {report['source_transactions']} ({100*report['transaction_retention']:.2f}%)",
        f"call frames: {calls['total_frames']} total; {calls['mapped_native_frames']} native; {calls['mapped_system_frames']} system; {calls['inlined_delegatecall_frames']} inlined delegate; {calls['background_fallback_frames']} fallback",
        f"successful reviewed-state transactions: {txc['transactions_with_semantic_action']} / {report['source_transactions']} ({100*txc['semantic_transaction_coverage']:.2f}%)",
        f"  fully semantic state: {txc['fully_semantic_transactions']}",
        f"  mixed state+fallback: {txc['mixed_semantic_fallback_transactions']}",
        f"  reviewed state-touch incl. reverted: {txc['transactions_with_reviewed_state_touch']} ({100*txc['reviewed_state_touch_coverage']:.2f}%)",
        f"  reviewed pure-only: {txc['reviewed_pure_only_transactions']}",
        f"  background only: {txc['background_only_transactions']}",
        "",
        f"source conflict-pair coverage: {source['selected_family_unique_conflict_pairs']} / {source['total_unique_conflict_pairs']} ({100*source['coverage']:.2f}%)",
        f"source storage-access coverage: {storage['selected_family_access_records']} / {storage['total_access_records']} ({100*storage['access_record_coverage']:.2f}%)",
        f"native instances observed: {report['native_instances']}",
        "",
        "Concrete source accesses were used only by the independent coverage audit; they are absent from native-plan.jsonl.",
    ]) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--thin-corpus", type=Path, required=True)
    ap.add_argument("--call-cache", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--mapping-candidates", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--source-coverage", type=Path, required=True)
    ap.add_argument("--output-dir", type=Path, required=True)
    ns = ap.parse_args()

    frozen = read_json(ns.family_map)
    validate_frozen_map(frozen)
    resolver = FamilyResolver(frozen, load_code_cache(ns.code_cache), read_json(ns.mapping_candidates))
    audit = read_json(ns.source_coverage)
    ns.output_dir.mkdir(parents=True, exist_ok=True)
    plan_path = ns.output_dir / "native-plan.jsonl"

    block_count = tx_count = 0
    fully = mixed = background = pure_only = 0
    state_touch_txs = 0
    total_frames = mapped = system = inline = fallback = opaque = 0
    successful_state_frames = state_touch_frames = pure_frames = reverted_state_frames = 0
    system_counts = Counter()
    instances: dict[str, dict] = {}

    with plan_path.open("w", encoding="utf-8") as out:
        for block in iter_blocks(ns.thin_corpus):
            bn = int(block["block_number"])
            call_path = ns.call_cache / f"{bn}.json"
            if not call_path.exists():
                raise SystemExit(f"missing callTracer cache: {call_path}")
            calls = read_json(call_path).get("transactions") or []
            source_txs = block.get("transactions") or []
            if len(calls) != len(source_txs):
                raise SystemExit(f"block {bn}: thin tx={len(source_txs)} callTracer tx={len(calls)}")
            plan_txs = []
            for tx, traced in zip(source_txs, calls):
                actions = translate_call_tree(traced.get("result") or {}, resolver)
                semantic_here = any(is_successful_reviewed_state_action(a) for a in actions)
                state_touch_here = any(is_reviewed_state_touch(a) for a in actions)
                pure_here = any(is_successful_reviewed_pure(a) for a in actions)
                fallback_here = any(
                    a.get("translation_status") == "background-fallback" or a.get("dispatch") == "mapped-opaque-selector"
                    for a in actions
                )
                if state_touch_here:
                    state_touch_txs += 1
                if semantic_here and not fallback_here:
                    cls = "fully-semantic"; fully += 1
                elif semantic_here:
                    cls = "mixed-semantic-fallback"; mixed += 1
                else:
                    cls = "background-only"; background += 1
                    if pure_here and not state_touch_here:
                        pure_only += 1
                total_frames += len(actions)
                mapped += sum(
                    a.get("translation_status") == "mapped-native-call" and a.get("dispatch") != "mapped-opaque-selector"
                    for a in actions
                )
                system += sum(a.get("translation_status") == SYSTEM_TRANSLATION_STATUS for a in actions)
                inline += sum(a.get("translation_status") == "inlined-delegatecall" for a in actions)
                fallback += sum(a.get("translation_status") == "background-fallback" for a in actions)
                opaque += sum(a.get("dispatch") == "mapped-opaque-selector" for a in actions)
                successful_state_frames += sum(is_successful_reviewed_state_action(a) for a in actions)
                state_touch_frames += sum(is_reviewed_state_touch(a) for a in actions)
                pure_frames += sum(is_successful_reviewed_pure(a) for a in actions)
                reverted_state_frames += sum(bool(a.get("failed_frame")) and is_reviewed_state_touch(a) for a in actions)
                system_counts.update(a.get("system_action_kind") for a in actions if a.get("system_action_kind"))
                for a in actions:
                    iid = a.get("native_instance_id")
                    native = a.get("native_code_family")
                    owner = a.get("storage_context_address")
                    profile = a.get("ethereum_profile_family")
                    if iid and native and owner:
                        instances.setdefault(str(iid), {
                            "native_instance_id": str(iid),
                            "native_code_family": str(native),
                            "ethereum_profile_family": profile,
                            "source_storage_owner": owner,
                            "observed_in_call_plan": True,
                        })
                plan_txs.append({
                    "tx_index": int(tx["tx_index"]),
                    "tx_hash": str(tx["tx_hash"]).lower(),
                    "from": str(tx.get("from") or "0x").lower(),
                    "to": str(tx.get("to") or "<create>").lower(),
                    "selector": str(tx.get("selector") or "0x").lower(),
                    "value": str(tx.get("value") or "0x0").lower(),
                    "gas_used_compute_proxy": int(tx.get("gas_used", tx.get("opcode_steps", 0)) or 0),
                    "source_failed": bool(tx.get("failed")),
                    "translation_class": cls,
                    "native_actions": actions,
                })
                tx_count += 1
            row = {
                "schema_version": 2,
                "block_number": bn,
                "block_hash": str(block.get("block_hash") or "").lower(),
                "timestamp": int(block.get("timestamp", 0) or 0),
                "transactions": plan_txs,
            }
            out.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
            block_count += 1
            if block_count % 100 == 0:
                print(f"native plan blocks={block_count} tx={tx_count} frames={total_frames}", flush=True)

    source_blocks = int(audit.get("source_blocks", block_count))
    source_txs_total = int(audit.get("source_transactions", tx_count))
    semantic_txs = fully + mixed
    coverage = {
        "schema_version": 3,
        "dataset": frozen.get("dataset", "vegeta-s1"),
        "source_blocks": source_blocks,
        "blocks_retained": block_count,
        "source_transactions": source_txs_total,
        "transactions_retained": tx_count,
        "transaction_retention": tx_count / source_txs_total if source_txs_total else 1.0,
        "calls": {
            "total_frames": total_frames,
            "mapped_native_frames": mapped,
            "mapped_opaque_selector_frames": opaque,
            "mapped_system_frames": system,
            "semantic_frames": successful_state_frames,
            "semantic_frame_coverage": successful_state_frames / total_frames if total_frames else 1.0,
            "reviewed_state_touch_frames": state_touch_frames,
            "reviewed_state_touch_frame_coverage": state_touch_frames / total_frames if total_frames else 1.0,
            "reviewed_pure_frames": pure_frames,
            "reviewed_reverted_state_frames": reverted_state_frames,
            "inlined_delegatecall_frames": inline,
            "background_fallback_frames": fallback,
            "system_actions": {
                "plain_value_transfer": int(system_counts["plain-value-transfer"]),
                "ethereum_precompile": int(system_counts["ethereum-precompile"]),
                "empty_code_noop": int(system_counts["empty-code-noop"]),
            },
        },
        "transaction_semantic_coverage": {
            "fully_semantic_transactions": fully,
            "mixed_semantic_fallback_transactions": mixed,
            "background_only_transactions": background,
            "reviewed_pure_only_transactions": pure_only,
            "transactions_with_semantic_action": semantic_txs,
            "semantic_transaction_coverage": semantic_txs / source_txs_total if source_txs_total else 1.0,
            "transactions_with_reviewed_state_touch": state_touch_txs,
            "reviewed_state_touch_coverage": state_touch_txs / source_txs_total if source_txs_total else 1.0,
        },
        "source_conflict_coverage": audit["source_conflict_coverage"],
        "block_balanced_conflict_coverage": audit["block_balanced_conflict_coverage"],
        "storage_access_coverage": audit["storage_access_coverage"],
        "native_code_families": len(frozen.get("native_code_families") or {}),
        "native_instances": len(instances),
        "implementation_readiness": implementation_readiness(frozen, ROOT),
        "native_topology_fidelity": {"status": "not-measured-preexecution"},
        "prediction_leakage_guard": "native-plan.jsonl was built from thin-corpus + callTracer and contains no source reads/writes",
    }
    atomic_json(ns.output_dir / "translation-coverage.json", coverage)
    (ns.output_dir / "translation-coverage.txt").write_text(render(coverage), encoding="utf-8")
    atomic_json(ns.output_dir / "native-instance-catalog.json", {
        "schema_version": 1,
        "namespace_rule": "one native instance per distinct Ethereum storage-owner address",
        "instances": sorted(instances.values(), key=lambda x: (x["native_code_family"], x["source_storage_owner"])),
        "total_instances": len(instances),
    })
    # Planner-known semantic actions are self-describing; an empty selector fallback map is valid.
    # Extra selector mappings can be added later only after source review rather than guessed from S1.
    atomic_json(ns.output_dir / "selector-semantic-map.json", {"schema_version": 1, "dataset": frozen.get("dataset"), "rules": []})
    atomic_json(ns.output_dir / "manifest.json", {
        "schema_version": 3,
        "dataset": frozen.get("dataset", "vegeta-s1"),
        "source_corpus": str(ns.thin_corpus),
        "source_coverage": str(ns.source_coverage),
        "family_map": str(ns.family_map),
        "family_map_sha256": hashlib.sha256(ns.family_map.read_bytes()).hexdigest(),
        "call_trace_semantics": "geth-callTracer-v1",
        "oracle_accesses_embedded_in_plan": False,
    })
    print(render(coverage), end="")
    print(f"wrote {plan_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
