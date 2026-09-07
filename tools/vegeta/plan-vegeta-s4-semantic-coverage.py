#!/usr/bin/env python3
"""Plan S4 semantic-family expansion from transaction-level unresolved blocker sets.

This is an offline planning aid.  It never mutates the reviewed/frozen family map.
The key unit is the *complete unresolved family set* for a source transaction. Strict
fully-mapped transaction gas and conflict-relevant storage-access coverage are retained as
planning/diagnostic references. The family-freeze hard target is source conflict coverage.
"""
from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from native_s3_planner_compat import FamilyResolver, load_code_cache
from vegeta_corpus import iter_blocks, storage_contract


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


@dataclass
class BlockerInfo:
    blocker_id: str
    kind: str
    runtime_code_family: str | None = None
    address: str | None = None
    owners: set[str] = field(default_factory=set)
    gas_attributions: int = 0
    transactions: int = 0
    access_records: int = 0
    conflict_relevant_access_records: int = 0
    single_family_conflict_gain_pairs: int = 0


def seed_indexes(seed_doc: dict) -> tuple[dict[str, dict], dict[str, dict]]:
    by_family: dict[str, dict] = {}
    by_address: dict[str, dict] = {}
    for row in seed_doc.get("decisions") or []:
        family = str(row.get("runtime_code_family") or "")
        address = str(row.get("address") or "").lower()
        if family:
            by_family[family] = row
        if address:
            by_address[address] = row
    return by_family, by_address


def blocker_hint(info: BlockerInfo, by_family: dict[str, dict], by_address: dict[str, dict]) -> dict:
    seed = None
    if info.runtime_code_family:
        seed = by_family.get(info.runtime_code_family)
    if seed is None and info.address:
        seed = by_address.get(info.address.lower())
    if seed is None:
        return {
            "review_priority": None,
            "identity_hint": None,
            "suggested_native_family": None,
            "implementation_disposition": "unclassified-review-required",
            "semantic_notes": None,
        }
    suggested = seed.get("suggested_native_family")
    return {
        "review_priority": seed.get("priority"),
        "identity_hint": seed.get("identity_hint"),
        "suggested_native_family": suggested,
        "implementation_disposition": (
            "review-seed-existing-native-candidate" if suggested else "review-seed-new-or-manual"
        ),
        "semantic_notes": seed.get("semantic_notes"),
    }


def blocker_record(info: BlockerInfo, by_family: dict[str, dict], by_address: dict[str, dict]) -> dict:
    out = {
        "blocker_id": info.blocker_id,
        "kind": info.kind,
        "runtime_code_family": info.runtime_code_family,
        "address": info.address,
        "owner_count": len(info.owners),
        "owners": sorted(info.owners)[:50],
        "gas_attributions": info.gas_attributions,
        "transactions": info.transactions,
        "access_records": info.access_records,
        "conflict_relevant_access_records": info.conflict_relevant_access_records,
        "single_family_conflict_gain_pairs": info.single_family_conflict_gain_pairs,
    }
    out.update(blocker_hint(info, by_family, by_address))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--mapping-candidates", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--review-seeds", type=Path)
    ap.add_argument("--coverage", type=Path)
    ap.add_argument("--clusters-output", type=Path, required=True)
    ap.add_argument("--plan-output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--top-clusters", type=int, default=100)
    ap.add_argument("--top-families", type=int, default=100)
    ap.add_argument(
        "--target-conflict-relevant-access", "--target-storage-access",
        dest="target_conflict_relevant_access", type=float, default=0.90,
        help="target coverage of accesses whose storage owner participates in an observed cross-transaction conflict; --target-storage-access is a legacy alias",
    )
    ap.add_argument("--target-conflict", type=float, default=0.95)
    ap.add_argument("--target-state-gas", type=float, default=None, help=argparse.SUPPRESS)
    ap.add_argument("--max-greedy-steps", type=int, default=100)
    ns = ap.parse_args()
    if ns.top_clusters <= 0 or ns.top_families <= 0 or ns.max_greedy_steps <= 0:
        raise SystemExit("top/max values must be positive")
    if not 0 < ns.target_conflict_relevant_access <= 1:
        raise SystemExit("--target-conflict-relevant-access must be in (0,1]")
    if not 0 < ns.target_conflict <= 1:
        raise SystemExit("--target-conflict must be in (0,1]")
    if ns.target_state_gas is not None and not 0 < ns.target_state_gas <= 1:
        raise SystemExit("--target-state-gas must be in (0,1]")

    family_map = read_json(ns.family_map)
    code_cache = load_code_cache(ns.code_cache)
    mapping_candidates = read_json(ns.mapping_candidates)
    resolver = FamilyResolver(family_map, code_cache, mapping_candidates)
    seed_doc = read_json(ns.review_seeds) if ns.review_seeds and ns.review_seeds.exists() else {"decisions": []}
    seeds_by_family, seeds_by_address = seed_indexes(seed_doc)

    blockers: dict[str, BlockerInfo] = {}
    cluster_gas: Counter[tuple[str, ...]] = Counter()
    cluster_txs: Counter[tuple[str, ...]] = Counter()
    cluster_unmapped_access_records: Counter[tuple[str, ...]] = Counter()
    total_source_gas = 0
    source_state_gas = 0
    source_state_txs = 0
    currently_covered_gas = 0
    currently_covered_txs = 0
    total_state_access_records = 0
    currently_covered_access_records = 0
    owner_access_records: Counter[str] = Counter()
    mapped_storage_owners: set[str] = set()
    conflict_relevant_owners: set[str] = set()
    blocks = 0
    total_txs = 0
    total_conflict_pairs = 0
    currently_covered_conflict_pairs = 0
    global_all_pairs: set[tuple[int, int, int]] = set()
    global_mapped_pairs: set[tuple[int, int, int]] = set()
    blocker_global_pairs: dict[str, set[tuple[int, int, int]]] = defaultdict(set)

    def blocker_for_owner(owner: str) -> BlockerInfo:
        profile = resolver.profile_for_storage_context(owner)
        if profile:
            blocker_id = "family:" + profile
            info = blockers.get(blocker_id)
            if info is None:
                info = blockers[blocker_id] = BlockerInfo(
                    blocker_id=blocker_id,
                    kind="runtime-family",
                    runtime_code_family=profile,
                )
        else:
            blocker_id = "owner:" + owner.lower()
            info = blockers.get(blocker_id)
            if info is None:
                info = blockers[blocker_id] = BlockerInfo(
                    blocker_id=blocker_id,
                    kind="unresolved-owner",
                    address=owner.lower(),
                )
        info.owners.add(owner.lower())
        return info

    for block in iter_blocks(ns.corpus):
        blocks += 1
        block_number = int(block.get("block_number", blocks))
        readers: dict[str, set[int]] = defaultdict(set)
        writers: dict[str, set[int]] = defaultdict(set)
        for idx, tx in enumerate(block.get("transactions") or []):
            total_txs += 1
            tx_gas = int(tx.get("gas_used", 0) or 0)
            total_source_gas += tx_gas
            owners: set[str] = set()
            owner_access_count: Counter[str] = Counter()
            for key in tx.get("reads") or []:
                key = str(key)
                readers[key].add(idx)
                raw = storage_contract(key)
                if raw:
                    owner = "0x" + raw
                    owners.add(owner)
                    owner_access_count[owner] += 1
                    owner_access_records[owner] += 1
            for key in tx.get("writes") or []:
                key = str(key)
                writers[key].add(idx)
                raw = storage_contract(key)
                if raw:
                    owner = "0x" + raw
                    owners.add(owner)
                    owner_access_count[owner] += 1
                    owner_access_records[owner] += 1
            if not owners:
                continue
            source_state_txs += 1
            source_state_gas += tx_gas
            tx_access_records = sum(owner_access_count.values())
            total_state_access_records += tx_access_records
            unresolved: set[str] = set()
            blocker_access_records: Counter[str] = Counter()
            for owner in owners:
                _, native = resolver.native_family_for_storage_context(owner)
                if native:
                    currently_covered_access_records += owner_access_count[owner]
                    mapped_storage_owners.add(owner)
                    continue
                info = blocker_for_owner(owner)
                unresolved.add(info.blocker_id)
                blocker_access_records[info.blocker_id] += owner_access_count[owner]
            for blocker_id in unresolved:
                info = blockers[blocker_id]
                # Gas/transaction attribution is per transaction/family, not per owner, so
                # multiple storage owners sharing one runtime family do not double-count it.
                info.gas_attributions += tx_gas
                info.transactions += 1
                info.access_records += blocker_access_records[blocker_id]
            if not unresolved:
                currently_covered_gas += tx_gas
                currently_covered_txs += 1
            else:
                key = tuple(sorted(unresolved))
                cluster_gas[key] += tx_gas
                cluster_txs[key] += 1
                cluster_unmapped_access_records[key] += sum(blocker_access_records.values())

        # Exact current coverage and exact *single-family* conflict gain for each candidate.
        # Candidate gains are not additive because the same pair can be attributable to
        # multiple storage owners/families; the review gate remains the exact authority.
        all_pairs: set[tuple[int, int]] = set()
        mapped_pairs: set[tuple[int, int]] = set()
        blocker_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
        for key in set(readers) | set(writers):
            ws = writers.get(key, set())
            if not ws:
                continue
            touched = sorted(readers.get(key, set()) | ws)
            pairs = {
                (left, right)
                for pos, left in enumerate(touched)
                for right in touched[pos + 1 :]
                if left in ws or right in ws
            }
            if not pairs:
                continue
            all_pairs.update(pairs)
            raw = storage_contract(key)
            if not raw:
                continue
            owner = "0x" + raw
            conflict_relevant_owners.add(owner)
            _, native = resolver.native_family_for_storage_context(owner)
            if native:
                mapped_pairs.update(pairs)
            else:
                info = blocker_for_owner(owner)
                blocker_pairs[info.blocker_id].update(pairs)
        total_conflict_pairs += len(all_pairs)
        currently_covered_conflict_pairs += len(mapped_pairs)
        global_all_pairs.update((block_number, left, right) for left, right in all_pairs)
        global_mapped_pairs.update((block_number, left, right) for left, right in mapped_pairs)
        for blocker_id, pairs in blocker_pairs.items():
            blockers[blocker_id].single_family_conflict_gain_pairs += len(pairs - mapped_pairs)
            blocker_global_pairs[blocker_id].update((block_number, left, right) for left, right in pairs)

        if blocks % 250 == 0:
            pct = 100 * currently_covered_gas / source_state_gas if source_state_gas else 100.0
            print(
                f"planner blocks={blocks} tx={total_txs} blockers={len(blockers)} "
                f"state_gas_covered={pct:.2f}%",
                flush=True,
            )

    # Canonicalize conflict totals from globally unique (block, tx-left, tx-right) ids.
    total_conflict_pairs = len(global_all_pairs)
    currently_covered_conflict_pairs = len(global_mapped_pairs)

    # Conflict-relevant semantic surface: every concrete storage access whose owner
    # participates in at least one observed cross-transaction source conflict.
    # This remains a useful diagnostic/planning reference, but is not a family-freeze gate.
    total_conflict_relevant_access_records = sum(owner_access_records[o] for o in conflict_relevant_owners)
    currently_covered_conflict_relevant_access_records = sum(
        owner_access_records[o] for o in conflict_relevant_owners if o in mapped_storage_owners
    )
    for info in blockers.values():
        info.conflict_relevant_access_records = sum(
            owner_access_records[o] for o in info.owners if o in conflict_relevant_owners
        )

    current_gas_coverage = currently_covered_gas / source_state_gas if source_state_gas else 1.0
    current_storage_access_coverage = (
        currently_covered_access_records / total_state_access_records if total_state_access_records else 1.0
    )
    current_conflict_relevant_access_coverage = (
        currently_covered_conflict_relevant_access_records / total_conflict_relevant_access_records
        if total_conflict_relevant_access_records else 1.0
    )
    current_conflict_coverage = (
        currently_covered_conflict_pairs / total_conflict_pairs if total_conflict_pairs else 1.0
    )

    top_cluster_rows = []
    for blocker_ids, gas_used in cluster_gas.most_common(ns.top_clusters):
        txs = cluster_txs[blocker_ids]
        top_cluster_rows.append(
            {
                "blocker_ids": list(blocker_ids),
                "blocker_count": len(blocker_ids),
                "transactions": txs,
                "gas_used": gas_used,
                "source_state_gas_fraction": gas_used / source_state_gas if source_state_gas else 0.0,
                "unmapped_access_records": cluster_unmapped_access_records[blocker_ids],
                "blockers": [blocker_record(blockers[b], seeds_by_family, seeds_by_address) for b in blocker_ids],
            }
        )

    cluster_doc = {
        "schema_version": 1,
        "dataset": family_map.get("dataset"),
        "definition": (
            "Each cluster groups source state-touching transactions by the complete set of currently "
            "unresolved runtime families/owners. A transaction's gas becomes fully covered only when every "
            "blocker in its cluster is reviewed/mapped."
        ),
        "source_blocks": blocks,
        "source_transactions": total_txs,
        "source_state_transactions": source_state_txs,
        "source_state_gas_used": source_state_gas,
        "currently_fully_mapped_state_transactions": currently_covered_txs,
        "currently_fully_mapped_state_gas_used": currently_covered_gas,
        "current_state_gas_coverage": current_gas_coverage,
        "total_state_access_records": total_state_access_records,
        "currently_mapped_state_access_records": currently_covered_access_records,
        "current_storage_access_coverage_diagnostic": current_storage_access_coverage,
        "current_all_storage_access_coverage_diagnostic": current_storage_access_coverage,
        "total_conflict_relevant_access_records": total_conflict_relevant_access_records,
        "currently_mapped_conflict_relevant_access_records": currently_covered_conflict_relevant_access_records,
        "current_conflict_relevant_access_coverage": current_conflict_relevant_access_coverage,
        "total_unresolved_blockers": len(blockers),
        "total_blocker_clusters": len(cluster_gas),
        "top_clusters": top_cluster_rows,
    }
    write_json(ns.clusters_output, cluster_doc)

    # Planning exposes both conflict closure and a conflict-relevant-access reference. The
    # family-freeze hard gate is source conflict-pair coverage; access and strict all-or-nothing
    # transaction gas remain diagnostics/planning references. Access records are disjoint across blocker ids, while
    # conflict pairs can overlap across families, so projected conflict gain is recomputed
    # exactly from the union of selected blocker-pair sets at every step.
    target_access_records = ns.target_conflict_relevant_access * total_conflict_relevant_access_records
    target_conflict_pairs = ns.target_conflict * total_conflict_pairs

    clusters_by_blocker: dict[str, list[tuple[str, ...]]] = defaultdict(list)
    for cluster in cluster_gas:
        for blocker_id in cluster:
            clusters_by_blocker[blocker_id].append(cluster)

    def build_gate_plan(mode: str) -> dict:
        selected: set[str] = set()
        covered_pairs = set(global_mapped_pairs)
        covered_access_records = currently_covered_conflict_relevant_access_records
        covered_all_access_records_diagnostic = currently_covered_access_records
        strict_unlocked_gas = 0
        strict_unlocked_txs = 0
        credited_clusters: set[tuple[str, ...]] = set()
        rows: list[dict] = []

        for step_no in range(1, ns.max_greedy_steps + 1):
            access_cov = (covered_access_records / total_conflict_relevant_access_records if total_conflict_relevant_access_records else 1.0)
            conflict_cov = len(covered_pairs) / total_conflict_pairs if total_conflict_pairs else 1.0
            if access_cov >= ns.target_conflict_relevant_access and conflict_cov >= ns.target_conflict:
                break

            remaining_access_deficit = max(target_access_records - covered_access_records, 0.0)
            remaining_conflict_deficit = max(target_conflict_pairs - len(covered_pairs), 0.0)
            candidates = [b for b in blockers if b not in selected]
            if not candidates:
                break

            scored: list[tuple[tuple[float, ...], str, int, int, float, float]] = []
            for blocker_id in candidates:
                access_gain = blockers[blocker_id].conflict_relevant_access_records
                conflict_gain = len(blocker_global_pairs.get(blocker_id, set()) - covered_pairs)
                access_fraction = (
                    min(access_gain, remaining_access_deficit) / remaining_access_deficit
                    if remaining_access_deficit > 0 else 0.0
                )
                conflict_fraction = (
                    min(conflict_gain, remaining_conflict_deficit) / remaining_conflict_deficit
                    if remaining_conflict_deficit > 0 else 0.0
                )
                if mode == "access-first":
                    score = (
                        (float(access_gain), conflict_fraction, float(conflict_gain), float(blockers[blocker_id].gas_attributions))
                        if remaining_access_deficit > 0
                        else (float(conflict_gain), access_fraction, float(access_gain), float(blockers[blocker_id].gas_attributions))
                    )
                elif mode == "conflict-first":
                    score = (
                        (float(conflict_gain), access_fraction, float(access_gain), float(blockers[blocker_id].gas_attributions))
                        if remaining_conflict_deficit > 0
                        else (float(access_gain), conflict_fraction, float(conflict_gain), float(blockers[blocker_id].gas_attributions))
                    )
                else:
                    # Equal weight is intentional: each term is normalized by the
                    # *remaining* deficit of its publication gate, so a family that
                    # closes 10% of either remaining deficit contributes equally.
                    balanced = access_fraction + conflict_fraction
                    score = (balanced, min(access_fraction, conflict_fraction), access_fraction, conflict_fraction, float(access_gain + conflict_gain))
                scored.append((score, blocker_id, access_gain, conflict_gain, access_fraction, conflict_fraction))

            scored.sort(key=lambda item: (item[0], item[1]), reverse=True)
            _, chosen, access_gain, conflict_gain, access_fraction, conflict_fraction = scored[0]
            selected.add(chosen)
            covered_access_records += access_gain
            covered_all_access_records_diagnostic += blockers[chosen].access_records
            new_pairs = blocker_global_pairs.get(chosen, set()) - covered_pairs
            covered_pairs.update(new_pairs)

            newly_unlocked_gas = 0
            newly_unlocked_txs = 0
            for cluster in clusters_by_blocker.get(chosen, []):
                if cluster in credited_clusters:
                    continue
                if all(b in selected for b in cluster):
                    credited_clusters.add(cluster)
                    newly_unlocked_gas += cluster_gas[cluster]
                    newly_unlocked_txs += cluster_txs[cluster]
            strict_unlocked_gas += newly_unlocked_gas
            strict_unlocked_txs += newly_unlocked_txs

            projected_access_cov = (covered_access_records / total_conflict_relevant_access_records if total_conflict_relevant_access_records else 1.0)
            projected_all_access_cov_diagnostic = (
                covered_all_access_records_diagnostic / total_state_access_records if total_state_access_records else 1.0
            )
            projected_conflict_cov = len(covered_pairs) / total_conflict_pairs if total_conflict_pairs else 1.0
            info = blockers[chosen]
            row = blocker_record(info, seeds_by_family, seeds_by_address)
            row.update(
                {
                    "step": step_no,
                    "plan_mode": mode,
                    "selection_reason": "conflict-and-access-planning-reference-closure",
                    "newly_covered_conflict_relevant_access_records": access_gain,
                    "newly_covered_storage_access_records": access_gain,  # legacy alias: now conflict-relevant
                    "newly_covered_all_storage_access_records_diagnostic": info.access_records,
                    "newly_covered_conflict_pairs": len(new_pairs),
                    "normalized_remaining_access_deficit_closed": access_fraction,
                    "normalized_remaining_conflict_deficit_closed": conflict_fraction,
                    "balanced_gate_score": access_fraction + conflict_fraction,
                    "newly_unlocked_gas_diagnostic": newly_unlocked_gas,
                    "newly_unlocked_transactions_diagnostic": newly_unlocked_txs,
                    # Backward-compatible aliases used by earlier local analysis snippets.
                    "newly_unlocked_gas": newly_unlocked_gas,
                    "newly_unlocked_transactions": newly_unlocked_txs,
                    "cumulative_projected_mapped_conflict_relevant_access_records": covered_access_records,
                    "cumulative_projected_conflict_relevant_access_coverage": projected_access_cov,
                    "cumulative_projected_mapped_storage_access_records": covered_access_records,  # legacy alias
                    "cumulative_projected_storage_access_coverage": projected_access_cov,  # legacy alias
                    "cumulative_projected_all_storage_access_coverage_diagnostic": projected_all_access_cov_diagnostic,
                    "cumulative_projected_covered_conflict_pairs": len(covered_pairs),
                    "cumulative_projected_conflict_coverage": projected_conflict_cov,
                    "cumulative_projected_fully_mapped_state_gas_used": currently_covered_gas + strict_unlocked_gas,
                    "cumulative_projected_state_gas_coverage": (
                        (currently_covered_gas + strict_unlocked_gas) / source_state_gas if source_state_gas else 1.0
                    ),
                    "remaining_storage_access_deficit_records": max(target_access_records - covered_access_records, 0.0),
                    "remaining_conflict_deficit_pairs": max(target_conflict_pairs - len(covered_pairs), 0.0),
                }
            )
            rows.append(row)

        final_access = (covered_access_records / total_conflict_relevant_access_records if total_conflict_relevant_access_records else 1.0)
        final_all_access_diagnostic = (
            covered_all_access_records_diagnostic / total_state_access_records if total_state_access_records else 1.0
        )
        final_conflict = len(covered_pairs) / total_conflict_pairs if total_conflict_pairs else 1.0
        final_strict_gas = (currently_covered_gas + strict_unlocked_gas) / source_state_gas if source_state_gas else 1.0
        return {
            "mode": mode,
            "steps": rows,
            "projected_conflict_relevant_access_coverage": final_access,
            "projected_storage_access_coverage": final_access,  # legacy alias
            "projected_all_storage_access_coverage_diagnostic": final_all_access_diagnostic,
            "projected_conflict_coverage": final_conflict,
            "projected_fully_mapped_state_gas_coverage_diagnostic": final_strict_gas,
            "conflict_relevant_access_target_reached": final_access >= ns.target_conflict_relevant_access,
            "storage_access_target_reached": final_access >= ns.target_conflict_relevant_access,  # legacy alias
            "conflict_target_reached": final_conflict >= ns.target_conflict,
            "both_targets_reached": final_access >= ns.target_conflict_relevant_access and final_conflict >= ns.target_conflict,
        }

    balanced_plan = build_gate_plan("balanced")
    access_plan = build_gate_plan("access-first")
    conflict_plan = build_gate_plan("conflict-first")

    # Retain the old complement-aware strict-gas optimizer only as a diagnostic.
    # It is intentionally disconnected from publication-gate ordering.
    def build_strict_gas_diagnostic(max_steps: int = 30) -> list[dict]:
        selected: set[str] = set()
        credited: set[tuple[str, ...]] = set()
        rows: list[dict] = []
        for step_no in range(1, min(max_steps, ns.max_greedy_steps) + 1):
            immediate: Counter[str] = Counter()
            groups: Counter[tuple[str, ...]] = Counter()
            for cluster, gas_used in cluster_gas.items():
                left = tuple(sorted(b for b in cluster if b not in selected))
                if not left:
                    continue
                if len(left) == 1:
                    immediate[left[0]] += gas_used
                elif len(left) in (2, 3):
                    groups[left] += gas_used
            available = [b for b in blockers if b not in selected]
            if not available:
                break
            best_single = max(available, key=lambda b: (immediate.get(b, 0), blockers[b].gas_attributions, b))
            best_single_gain = immediate.get(best_single, 0)
            best_group = None
            best_group_gas = 0
            best_group_per_family = 0.0
            for group, gas_used in groups.items():
                per_family = gas_used / len(group)
                if (per_family, gas_used, group) > (best_group_per_family, best_group_gas, best_group or tuple()):
                    best_group, best_group_gas, best_group_per_family = group, gas_used, per_family
            if best_group and best_group_per_family > best_single_gain:
                chosen = max((b for b in best_group if b not in selected), key=lambda b: (blockers[b].access_records, blockers[b].single_family_conflict_gain_pairs, b))
                reason = "complement-lookahead-diagnostic"
                group_out = list(best_group)
            else:
                chosen = best_single
                reason = "immediate-strict-gas-diagnostic"
                group_out = None
            selected.add(chosen)
            unlocked = 0
            for cluster in clusters_by_blocker.get(chosen, []):
                if cluster in credited:
                    continue
                if all(b in selected for b in cluster):
                    credited.add(cluster)
                    unlocked += cluster_gas[cluster]
            row = blocker_record(blockers[chosen], seeds_by_family, seeds_by_address)
            row.update({"step": step_no, "selection_reason": reason, "lookahead_group": group_out, "newly_unlocked_gas": unlocked})
            rows.append(row)
        return rows

    strict_gas_diagnostic_steps = build_strict_gas_diagnostic()

    family_rows = sorted(
        (blocker_record(info, seeds_by_family, seeds_by_address) for info in blockers.values()),
        key=lambda r: (
            -int(r["gas_attributions"]),
            -int(r["single_family_conflict_gain_pairs"]),
            str(r["blocker_id"]),
        ),
    )
    plan_doc = {
        "schema_version": 3,
        "dataset": family_map.get("dataset"),
        "method": (
            "Set-cover planning over the hard source-conflict target plus a conflict-relevant-access planning reference. "
            "The balanced/access-first/conflict-first plans expose the tradeoff, but only exact source conflict coverage and "
            "median conflict-bearing-block coverage gate family freeze. Strict fully mapped transaction gas is diagnostic only."
        ),
        "safety_note": (
            "Planner suggestions never modify the family map. Runtime-family equivalence does not imply semantic "
            "equivalence; every selected family still requires explicit review and the normal exact coverage gate."
        ),
        "target_conflict_relevant_access_coverage": ns.target_conflict_relevant_access,
        "target_storage_access_coverage": ns.target_conflict_relevant_access,  # legacy alias
        "target_conflict_coverage": ns.target_conflict,
        "deprecated_target_state_gas_reference": ns.target_state_gas,
        "source_state_gas_used": source_state_gas,
        "source_state_transactions": source_state_txs,
        "total_state_access_records": total_state_access_records,
        "current_mapped_state_access_records": currently_covered_access_records,
        "current_storage_access_coverage_diagnostic": current_storage_access_coverage,
        "current_all_storage_access_coverage_diagnostic": current_storage_access_coverage,
        "total_conflict_relevant_access_records": total_conflict_relevant_access_records,
        "currently_mapped_conflict_relevant_access_records": currently_covered_conflict_relevant_access_records,
        "current_conflict_relevant_access_coverage": current_conflict_relevant_access_coverage,
        "current_fully_mapped_state_gas_used": currently_covered_gas,
        "current_fully_mapped_state_transactions": currently_covered_txs,
        "current_state_gas_coverage": current_gas_coverage,
        "current_conflict_coverage": current_conflict_coverage,
        "total_conflict_pairs": total_conflict_pairs,
        "current_covered_conflict_pairs": currently_covered_conflict_pairs,
        "remaining_conflict_relevant_access_deficit_records": max(target_access_records - currently_covered_conflict_relevant_access_records, 0.0),
        "remaining_storage_access_deficit_records": max(target_access_records - currently_covered_conflict_relevant_access_records, 0.0),  # legacy alias
        "remaining_conflict_deficit_pairs": max(target_conflict_pairs - currently_covered_conflict_pairs, 0.0),
        "balanced_plan": balanced_plan,
        "access_first_plan": access_plan,
        "conflict_first_plan": conflict_plan,
        "strict_gas_diagnostic_steps": strict_gas_diagnostic_steps,
        # Backward-compatible alias: existing scripts that read greedy_steps now receive
        # the balanced planning-reference plan.
        "greedy_steps": balanced_plan["steps"],
        "projected_conflict_relevant_access_coverage_after_steps": balanced_plan["projected_conflict_relevant_access_coverage"],
        "projected_storage_access_coverage_after_steps": balanced_plan["projected_conflict_relevant_access_coverage"],  # legacy alias
        "projected_all_storage_access_coverage_after_steps_diagnostic": balanced_plan["projected_all_storage_access_coverage_diagnostic"],
        "projected_conflict_coverage_after_steps": balanced_plan["projected_conflict_coverage"],
        "projected_state_gas_coverage_after_steps_diagnostic": balanced_plan["projected_fully_mapped_state_gas_coverage_diagnostic"],
        "target_reached_by_plan": balanced_plan["conflict_target_reached"],
        "all_planning_references_reached_by_plan": balanced_plan["both_targets_reached"],
        "top_blockers_by_attributed_gas": family_rows[: ns.top_families],
    }
    if ns.coverage and ns.coverage.exists():
        audit = read_json(ns.coverage)
        reported_gas = (audit.get("gas_weighted_family_coverage") or {}).get(
            "fully_selected_family_state_gas_coverage"
        )
        reported_access = (audit.get("conflict_relevant_storage_access_coverage") or {}).get("access_record_coverage")
        reported_all_access = (audit.get("storage_access_coverage") or {}).get("access_record_coverage")
        plan_doc["coverage_audit_cross_check"] = {
            "reported_conflict_relevant_access_coverage": reported_access,
            "planner_conflict_relevant_access_coverage": current_conflict_relevant_access_coverage,
            "conflict_relevant_access_absolute_difference": (
                abs(float(reported_access) - current_conflict_relevant_access_coverage) if reported_access is not None else None
            ),
            "reported_all_storage_access_coverage_diagnostic": reported_all_access,
            "planner_all_storage_access_coverage_diagnostic": current_storage_access_coverage,
            "reported_state_gas_coverage_diagnostic": reported_gas,
            "planner_state_gas_coverage_diagnostic": current_gas_coverage,
            "state_gas_absolute_difference": (
                abs(float(reported_gas) - current_gas_coverage) if reported_gas is not None else None
            ),
        }

    write_json(ns.plan_output, plan_doc)

    lines = [
        "Vegeta S4 semantic coverage planner",
        "",
        f"current conflict-relevant storage-access coverage: {currently_covered_conflict_relevant_access_records}/{total_conflict_relevant_access_records} ({100*current_conflict_relevant_access_coverage:.2f}%)",
        f"current all storage-access coverage (diagnostic): {currently_covered_access_records}/{total_state_access_records} ({100*current_storage_access_coverage:.2f}%)",
        f"current fully mapped state gas (conservative diagnostic): {currently_covered_gas}/{source_state_gas} ({100*current_gas_coverage:.2f}%)",
        f"current conflict coverage: {currently_covered_conflict_pairs}/{total_conflict_pairs} ({100*current_conflict_coverage:.2f}%)",
        f"remaining conflict-relevant access deficit to {100*ns.target_conflict_relevant_access:.1f}%: {max(target_access_records-currently_covered_conflict_relevant_access_records,0):.0f} records",
        f"remaining conflict deficit to {100*ns.target_conflict:.1f}%: {max(target_conflict_pairs-currently_covered_conflict_pairs,0):.0f} pairs",
        f"unresolved blocker families/owners: {len(blockers)}",
        f"unresolved transaction blocker clusters: {len(cluster_gas)}",
        "",
        "Top unresolved blocker clusters by source-state gas (diagnostic):",
    ]
    for row in top_cluster_rows[: min(20, ns.top_clusters)]:
        short = ",".join(x.removeprefix("family:").removeprefix("owner:")[:12] for x in row["blocker_ids"])
        lines.append(
            f"  blockers={row['blocker_count']:2d} tx={row['transactions']:6d} gas={row['gas_used']:12d} "
            f"({100*row['source_state_gas_fraction']:.2f}%) accesses={row['unmapped_access_records']:8d} [{short}]"
        )

    def append_plan(title: str, plan: dict, limit: int = 40) -> None:
        lines.extend(["", title])
        for row in plan["steps"][:limit]:
            hint = row.get("identity_hint") or row.get("runtime_code_family") or row.get("address") or row["blocker_id"]
            suggestion = row.get("suggested_native_family") or "manual/new"
            lines.append(
                f"  {row['step']:2d}. {hint} -> {suggestion}: "
                f"relevant-access+={row['newly_covered_conflict_relevant_access_records']} "
                f"conflict+={row['newly_covered_conflict_pairs']} "
                f"relevant-access-cumulative={100*row['cumulative_projected_conflict_relevant_access_coverage']:.2f}% "
                f"all-access-diagnostic={100*row['cumulative_projected_all_storage_access_coverage_diagnostic']:.2f}% "
                f"conflict-cumulative={100*row['cumulative_projected_conflict_coverage']:.2f}% "
                f"gate-score={row['balanced_gate_score']:.4f} "
                f"strict-gas-unlock={row['newly_unlocked_gas_diagnostic']}"
            )
        lines.append(
            f"  projected: relevant-access={100*plan['projected_conflict_relevant_access_coverage']:.2f}% "
            f"all-access-diagnostic={100*plan['projected_all_storage_access_coverage_diagnostic']:.2f}% "
            f"conflict={100*plan['projected_conflict_coverage']:.2f}% "
            f"strict-gas={100*plan['projected_fully_mapped_state_gas_coverage_diagnostic']:.2f}% "
            f"both-planning-refs={'YES' if plan['both_targets_reached'] else 'NO'}"
        )

    append_plan("Balanced conflict/access planning-reference plan:", balanced_plan)
    append_plan("Access-first plan:", access_plan, 25)
    append_plan("Conflict-first plan:", conflict_plan, 25)

    lines.extend(["", "Strict fully-mapped gas diagnostic plan (NOT used for family-freeze ordering):"])
    for row in strict_gas_diagnostic_steps[:20]:
        hint = row.get("identity_hint") or row.get("runtime_code_family") or row.get("address") or row["blocker_id"]
        lines.append(
            f"  {row['step']:2d}. {hint}: reason={row['selection_reason']} strict-gas-unlock={row['newly_unlocked_gas']}"
        )
    lines += [
        "",
        f"balanced conflict-relevant-access planning reference reached: {'YES' if balanced_plan['conflict_relevant_access_target_reached'] else 'NO'}",
        f"balanced HARD conflict target reached: {'YES' if balanced_plan['conflict_target_reached'] else 'NO'}",
        f"balanced both planning references reached: {'YES' if balanced_plan['both_targets_reached'] else 'NO'}",
        "",
        "Planning only: no family is executable until human review updates the review decisions/base map and the exact review gate passes.",
    ]
    text = "\n".join(lines) + "\n"
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text(text, encoding="utf-8")
    print(text, end="")
    print(f"wrote {ns.clusters_output}")
    print(f"wrote {ns.plan_output}")
    print(f"wrote {ns.text_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
