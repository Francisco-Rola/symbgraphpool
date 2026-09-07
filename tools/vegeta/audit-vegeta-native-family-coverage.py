#!/usr/bin/env python3
"""Stream source conflict/storage coverage for a frozen native-family mapping."""
from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path

from native_s3_planner_compat import FamilyResolver, block_balanced_conflict_metrics, load_code_cache
from vegeta_corpus import iter_blocks, storage_contract


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--mapping-candidates", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path)
    ap.add_argument("--top-unmapped", type=int, default=200, help="number of unmapped conflict owners to retain for expansion planning (default: 200)")
    ns = ap.parse_args()
    if ns.top_unmapped <= 0:
        raise SystemExit("--top-unmapped must be positive")
    frozen = read_json(ns.family_map)
    resolver = FamilyResolver(frozen, load_code_cache(ns.code_cache), read_json(ns.mapping_candidates))

    total_pairs = mapped_pairs = 0
    by_family_pairs: Counter[str] = Counter()
    total_access = mapped_access = mapped_txs = total_txs = blocks = 0
    total_state_owner_occurrences = mapped_state_owner_occurrences = 0
    owner_access_records: Counter[str] = Counter()
    mapped_storage_owners: set[str] = set()
    conflict_relevant_owners: set[str] = set()
    total_gas = source_state_gas = any_mapped_state_gas = fully_mapped_state_gas = 0
    source_state_txs = fully_mapped_state_txs = 0
    per_block = []
    unknown_owner_pairs: Counter[str] = Counter()
    unknown_owner_accesses: Counter[str] = Counter()
    unknown_owner_gas: Counter[str] = Counter()
    unknown_owner_transactions: Counter[str] = Counter()

    for block in iter_blocks(ns.corpus):
        blocks += 1
        bn = int(block["block_number"])
        # Reproduce the planner's pair semantics, but keep only this block's sets in memory.
        readers: dict[str, set[int]] = defaultdict(set)
        writers: dict[str, set[int]] = defaultdict(set)
        for idx, tx in enumerate(block.get("transactions") or []):
            total_txs += 1
            tx_gas = int(tx.get("gas_used", 0) or 0)
            total_gas += tx_gas
            tx_mapped = False
            tx_owners: set[str] = set()
            tx_mapped_owners: set[str] = set()
            for key in tx.get("reads") or []:
                total_access += 1
                readers[str(key)].add(idx)
                owner = storage_contract(str(key))
                owner_address = "0x" + owner if owner else None
                if owner_address:
                    tx_owners.add(owner_address)
                    owner_access_records[owner_address] += 1
                _, native = resolver.native_family_for_storage_context(owner_address)
                if native:
                    mapped_access += 1; tx_mapped = True
                    if owner_address:
                        tx_mapped_owners.add(owner_address)
                        mapped_storage_owners.add(owner_address)
                elif owner:
                    unknown_owner_accesses["0x" + owner] += 1
            for key in tx.get("writes") or []:
                total_access += 1
                writers[str(key)].add(idx)
                owner = storage_contract(str(key))
                owner_address = "0x" + owner if owner else None
                if owner_address:
                    tx_owners.add(owner_address)
                    owner_access_records[owner_address] += 1
                _, native = resolver.native_family_for_storage_context(owner_address)
                if native:
                    mapped_access += 1; tx_mapped = True
                    if owner_address:
                        tx_mapped_owners.add(owner_address)
                        mapped_storage_owners.add(owner_address)
                elif owner:
                    unknown_owner_accesses["0x" + owner] += 1
            if tx_owners:
                source_state_txs += 1
                total_state_owner_occurrences += len(tx_owners)
                mapped_state_owner_occurrences += len(tx_mapped_owners)
                source_state_gas += tx_gas
                if tx_mapped:
                    any_mapped_state_gas += tx_gas
                if tx_owners <= tx_mapped_owners:
                    fully_mapped_state_txs += 1
                    fully_mapped_state_gas += tx_gas
                for owner_address in tx_owners - tx_mapped_owners:
                    unknown_owner_gas[owner_address] += tx_gas
                    unknown_owner_transactions[owner_address] += 1
            if tx_mapped:
                mapped_txs += 1
        all_pairs: set[tuple[int, int]] = set()
        selected_pairs: set[tuple[int, int]] = set()
        family_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
        owner_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
        for key in set(readers) | set(writers):
            ws = writers.get(key, set())
            if not ws:
                continue
            touched = sorted(readers.get(key, set()) | ws)
            pairs = {
                (left, right)
                for pos, left in enumerate(touched)
                for right in touched[pos + 1:]
                if left in ws or right in ws
            }
            if not pairs:
                continue
            all_pairs.update(pairs)
            owner = storage_contract(key)
            if owner:
                owner_address = "0x" + owner
                owner_pairs[owner_address].update(pairs)
                conflict_relevant_owners.add(owner_address)
        for owner, pairs in owner_pairs.items():
            _, native = resolver.native_family_for_storage_context(owner)
            if native:
                selected_pairs.update(pairs)
                family_pairs[native].update(pairs)
            else:
                unknown_owner_pairs[owner] += len(pairs)
        total_pairs += len(all_pairs)
        mapped_pairs += len(selected_pairs)
        for family, pairs in family_pairs.items():
            by_family_pairs[family] += len(pairs)
        per_block.append({
            "block_number": bn,
            "total_conflict_pairs": len(all_pairs),
            "selected_family_conflict_pairs": len(selected_pairs),
            "coverage": len(selected_pairs) / len(all_pairs) if all_pairs else 1.0,
        })
        if blocks % 250 == 0:
            print(f"coverage blocks={blocks} tx={total_txs} pairs={total_pairs} mapped={mapped_pairs}", flush=True)

    source = {
        "total_unique_conflict_pairs": total_pairs,
        "selected_family_unique_conflict_pairs": mapped_pairs,
        "coverage": mapped_pairs / total_pairs if total_pairs else 1.0,
        "by_native_family": [
            {"native_code_family": family, "unique_conflict_pairs": count}
            for family, count in by_family_pairs.most_common()
        ],
        "per_block": per_block,
    }
    conflict_relevant_total_access = sum(owner_access_records[o] for o in conflict_relevant_owners)
    conflict_relevant_mapped_access = sum(
        owner_access_records[o] for o in conflict_relevant_owners if o in mapped_storage_owners
    )
    conflict_relevant_storage = {
        "definition": (
            "diagnostic reviewed-family coverage over concrete source storage accesses whose storage owner participates "
            "in at least one observed cross-transaction source conflict; S4 family freeze is gated on conflict coverage, not raw access coverage"
        ),
        "conflict_relevant_owner_count": len(conflict_relevant_owners),
        "total_access_records": conflict_relevant_total_access,
        "selected_family_access_records": conflict_relevant_mapped_access,
        "access_record_coverage": (
            conflict_relevant_mapped_access / conflict_relevant_total_access
            if conflict_relevant_total_access else 1.0
        ),
    }
    storage = {
        "definition": "diagnostic reviewed-family coverage over all concrete source storage accesses; non-conflicting long-tail state is included",
        "total_access_records": total_access,
        "selected_family_access_records": mapped_access,
        "access_record_coverage": mapped_access / total_access if total_access else 1.0,
        "total_state_owner_occurrences": total_state_owner_occurrences,
        "selected_family_state_owner_occurrences": mapped_state_owner_occurrences,
        "state_owner_occurrence_coverage": mapped_state_owner_occurrences / total_state_owner_occurrences if total_state_owner_occurrences else 1.0,
        "transactions_touching_selected_family_storage": mapped_txs,
        "transaction_coverage": mapped_txs / total_txs if total_txs else 1.0,
    }
    gas = {
        "definition": "conservative diagnostic over source transactions with concrete storage accesses; a transaction is fully covered only when every touched storage owner resolves to a reviewed native family. This metric is reported but is not the S4 family-freeze gate.",
        "total_source_gas_used": total_gas,
        "source_state_transaction_gas_used": source_state_gas,
        "source_state_transactions": source_state_txs,
        "transactions_with_any_selected_family_storage_gas_used": any_mapped_state_gas,
        "fully_selected_family_state_transaction_gas_used": fully_mapped_state_gas,
        "fully_selected_family_state_transactions": fully_mapped_state_txs,
        "any_selected_family_state_gas_coverage": any_mapped_state_gas / source_state_gas if source_state_gas else 1.0,
        "fully_selected_family_state_gas_coverage": fully_mapped_state_gas / source_state_gas if source_state_gas else 1.0,
        "fully_selected_family_state_transaction_coverage": fully_mapped_state_txs / source_state_txs if source_state_txs else 1.0,
    }
    out = {
        "schema_version": 1,
        "dataset": frozen.get("dataset"),
        "source_blocks": blocks,
        "source_transactions": total_txs,
        "source_conflict_coverage": source,
        "block_balanced_conflict_coverage": block_balanced_conflict_metrics(source),
        "conflict_relevant_storage_access_coverage": conflict_relevant_storage,
        "storage_access_coverage": storage,
        "gas_weighted_family_coverage": gas,
        "top_unmapped_conflict_owners": [
            {"address": owner, "owner_pair_attributions": count, "access_records": unknown_owner_accesses[owner], "gas_attributions": unknown_owner_gas[owner], "transactions": unknown_owner_transactions[owner]}
            for owner, count in unknown_owner_pairs.most_common(ns.top_unmapped)
        ],
        "top_unmapped_state_gas_owners": [
            {"address": owner, "gas_attributions": gas_used, "transactions": unknown_owner_transactions[owner], "access_records": unknown_owner_accesses[owner], "owner_pair_attributions": unknown_owner_pairs[owner]}
            for owner, gas_used in unknown_owner_gas.most_common(ns.top_unmapped)
        ],
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    text = [
        "Vegeta native-family source coverage audit",
        "",
        f"blocks: {blocks}", f"transactions: {total_txs}",
        f"conflict-pair coverage: {mapped_pairs}/{total_pairs} ({100*(mapped_pairs/total_pairs if total_pairs else 1):.2f}%)",
        f"conflict-relevant storage-access coverage (diagnostic): {conflict_relevant_mapped_access}/{conflict_relevant_total_access} ({100*(conflict_relevant_mapped_access/conflict_relevant_total_access if conflict_relevant_total_access else 1):.2f}%)",
        f"all storage-access coverage (diagnostic): {mapped_access}/{total_access} ({100*(mapped_access/total_access if total_access else 1):.2f}%)",
        f"state-owner occurrence coverage: {mapped_state_owner_occurrences}/{total_state_owner_occurrences} ({100*(mapped_state_owner_occurrences/total_state_owner_occurrences if total_state_owner_occurrences else 1):.2f}%)",
        f"transactions touching mapped storage: {mapped_txs}/{total_txs} ({100*(mapped_txs/total_txs if total_txs else 1):.2f}%)",
        f"fully mapped state-tx gas coverage (conservative diagnostic): {fully_mapped_state_gas}/{source_state_gas} ({100*(fully_mapped_state_gas/source_state_gas if source_state_gas else 1):.2f}%)",
        f"fully mapped state transactions: {fully_mapped_state_txs}/{source_state_txs} ({100*(fully_mapped_state_txs/source_state_txs if source_state_txs else 1):.2f}%)",
        "", "Top unmapped conflict owners:",
    ]
    text.extend(f"  {row['address']} pairs={row['owner_pair_attributions']} accesses={row['access_records']} gas_attr={row['gas_attributions']}" for row in out["top_unmapped_conflict_owners"][:20])
    text += ["", "Top unmapped source-state gas owners:"]
    text.extend(f"  {row['address']} gas_attr={row['gas_attributions']} tx={row['transactions']} pairs={row['owner_pair_attributions']}" for row in out["top_unmapped_state_gas_owners"][:20])
    text_value = "\n".join(text) + "\n"
    if ns.text_output:
        ns.text_output.parent.mkdir(parents=True, exist_ok=True)
        ns.text_output.write_text(text_value, encoding="utf-8")
    print(text_value, end="")
    print(f"wrote {ns.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
