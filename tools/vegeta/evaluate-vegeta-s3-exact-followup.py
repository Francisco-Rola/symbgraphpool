#!/usr/bin/env python3
"""Post-extraction Vegeta S3 fidelity follow-up for exact SLOAD/SSTORE ground truth.

This evaluation is intentionally post-hoc.  It never feeds concrete historical EVM keys back into
native planning, symbolic prediction, state priming, or execution.  It answers four questions after
native execution has already completed:

1. Does the previously frozen native-family mapping still cover the required share of source
   conflicts when the source ground truth is exact SLOAD/SSTORE rather than prestateTracer?
2. Are the explicitly disclosed fallback transactions topologically material?
3. Which source owners/profile families/keys explain the remaining native false negatives,
   especially false-negative edges that lie on a source longest path?
4. Which concrete source/native hot keys explain the residual hot-key-chain mismatch?

The script also emits an exact-vs-public-trace ablation when the original public-RPC corpus is
available.  The ablation is evaluation-only and should not be confused with scheduler inputs.
"""
from __future__ import annotations

import argparse
import csv
import json
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_EXACT = ROOT / "benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/corpus.jsonl"
DEFAULT_PUBLIC = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"
DEFAULT_NATIVE = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-execution/native-accesses.jsonl"
DEFAULT_PLAN = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl"
DEFAULT_CATALOG = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-instance-catalog.json"
DEFAULT_TRANSLATION_COVERAGE = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/translation-coverage.json"
DEFAULT_FINAL_MAPPING_SIMULATION = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/final-mapping-simulation.json"
DEFAULT_GATES = ROOT / "evaluation/vegeta/s3-exact-followup-gates.v1.json"
DEFAULT_OUT = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-execution/exact-followup"


def read_json(path: Path) -> dict:
    if not path.exists():
        raise FileNotFoundError(path)
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"expected JSON object in {path}")
    return value


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        raise FileNotFoundError(path)
    rows: list[dict] = []
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError(f"expected JSON object at {path}:{line_number}")
            rows.append(value)
    return rows


def parse_source_owner(key: str) -> str | None:
    parts = str(key).split("/")
    if len(parts) < 3 or parts[0] != "evm" or len(parts[1]) != 40:
        return None
    try:
        int(parts[1], 16)
    except ValueError:
        return None
    return "0x" + parts[1].lower()


def parse_source_slot(key: str) -> str | None:
    parts = str(key).split("/")
    if len(parts) < 3 or parts[0] != "evm":
        return None
    slot = parts[2].lower()
    try:
        int(slot, 16)
    except ValueError:
        return None
    return "0x" + slot


def source_rows(block: dict) -> list[tuple[set[str], set[str]]]:
    return [
        (set(tx.get("reads") or []), set(tx.get("writes") or []))
        for tx in block.get("transactions") or []
    ]


def native_identity(access: dict) -> str:
    kind = str(access.get("kind") or "")
    key = str(access.get("key_hex") or "")
    contract = str(access.get("contract") or "")
    if kind.startswith("storage_"):
        if kind == "storage_scan":
            return f"storage:{contract}:{key}:{access.get('range_end_hex') or ''}"
        return f"storage:{contract}:{key}"
    return f"bank:{key}"


def native_rows(block: dict, include_bank: bool = False) -> list[tuple[set[str], set[str]]]:
    rows: list[tuple[set[str], set[str]]] = []
    for tx in block.get("transactions") or []:
        reads: set[str] = set()
        writes: set[str] = set()
        tx_reverted = bool(tx.get("source_failed")) or tx.get("execution_status") == "reverted"
        for access in tx.get("accesses") or []:
            kind = str(access.get("kind") or "")
            if kind.startswith("bank_") and not include_bank:
                continue
            ident = native_identity(access)
            is_write = (
                kind in {"storage_write", "storage_remove", "bank_write"}
                and not tx_reverted
                and not bool(access.get("reverted"))
            )
            (writes if is_write else reads).add(ident)
        rows.append((reads, writes))
    return rows


def pairs_from_rw(rows: list[tuple[set[str], set[str]]]) -> set[tuple[int, int]]:
    readers: dict[str, set[int]] = defaultdict(set)
    writers: dict[str, set[int]] = defaultdict(set)
    for index, (reads, writes) in enumerate(rows):
        for key in reads:
            readers[key].add(index)
        for key in writes:
            writers[key].add(index)
    pairs: set[tuple[int, int]] = set()
    for key in set(readers) | set(writers):
        if not writers.get(key):
            continue
        touched = readers.get(key, set()) | writers[key]
        for left in touched:
            for right in writers[key]:
                if left == right:
                    continue
                pairs.add((min(left, right), max(left, right)))
    return pairs


def conflict_cause_keys(
    rows: list[tuple[set[str], set[str]]], left: int, right: int
) -> set[str]:
    left_reads, left_writes = rows[left]
    right_reads, right_writes = rows[right]
    shared = (left_reads | left_writes) & (right_reads | right_writes)
    return {key for key in shared if key in left_writes or key in right_writes}


def critical_path(n: int, pairs: set[tuple[int, int]]) -> int:
    if n <= 0:
        return 0
    predecessors: dict[int, list[int]] = defaultdict(list)
    for left, right in pairs:
        predecessors[right].append(left)
    dp = [1] * n
    for index in range(n):
        if predecessors[index]:
            dp[index] = 1 + max(dp[parent] for parent in predecessors[index])
    return max(dp, default=0)


def critical_path_edges(n: int, pairs: set[tuple[int, int]]) -> set[tuple[int, int]]:
    """Return edges that lie on at least one longest path in the canonical-order DAG."""
    if n <= 0 or not pairs:
        return set()
    successors: dict[int, list[int]] = defaultdict(list)
    predecessors: dict[int, list[int]] = defaultdict(list)
    for left, right in pairs:
        successors[left].append(right)
        predecessors[right].append(left)
    ending = [1] * n
    for index in range(n):
        if predecessors[index]:
            ending[index] = 1 + max(ending[parent] for parent in predecessors[index])
    starting = [1] * n
    for index in range(n - 1, -1, -1):
        if successors[index]:
            starting[index] = 1 + max(starting[child] for child in successors[index])
    longest = max(ending, default=0)
    return {
        (left, right)
        for left, right in pairs
        if ending[left] + starting[right] == longest
    }


def touches_by_key(rows: list[tuple[set[str], set[str]]]) -> dict[str, set[int]]:
    touched: dict[str, set[int]] = defaultdict(set)
    for index, (reads, writes) in enumerate(rows):
        for key in reads | writes:
            touched[key].add(index)
    return touched


def hot_key_chain(rows: list[tuple[set[str], set[str]]]) -> int:
    touched = touches_by_key(rows)
    return max((len(indices) for indices in touched.values()), default=0)


def ratio(numerator: int | float, denominator: int | float) -> float | None:
    return numerator / denominator if denominator else None


def f1_score(precision: float | None, recall: float | None) -> float | None:
    if precision is None or recall is None or precision + recall == 0:
        return None
    return 2.0 * precision * recall / (precision + recall)


def pct(value: float | None) -> str:
    return "n/a" if value is None else f"{100.0 * value:.2f}%"


def tx_hashes(block: dict) -> list[str]:
    return [str(tx.get("tx_hash") or tx.get("hash") or "").lower() for tx in block.get("transactions") or []]


def validate_block_alignment(source_blocks: list[dict], native_blocks: list[dict]) -> None:
    source_by_block = {int(block["block_number"]): block for block in source_blocks}
    native_by_block = {int(block["block_number"]): block for block in native_blocks}
    if set(source_by_block) != set(native_by_block):
        raise ValueError(
            f"block set mismatch: source={len(source_by_block)} native={len(native_by_block)}"
        )
    for block_number in sorted(source_by_block):
        source_hashes = tx_hashes(source_by_block[block_number])
        native_hashes = tx_hashes(native_by_block[block_number])
        if len(source_hashes) != len(native_hashes):
            raise ValueError(
                f"tx count mismatch in block {block_number}: "
                f"source={len(source_hashes)} native={len(native_hashes)}"
            )
        if all(source_hashes) and all(native_hashes) and source_hashes != native_hashes:
            raise ValueError(f"tx hash/order mismatch in block {block_number}")


def source_source_alignment(left_blocks: list[dict], right_blocks: list[dict]) -> None:
    left = {int(block["block_number"]): block for block in left_blocks}
    right = {int(block["block_number"]): block for block in right_blocks}
    if set(left) != set(right):
        raise ValueError(f"source corpus block mismatch: {len(left)} vs {len(right)}")
    for block_number in sorted(left):
        left_hashes = tx_hashes(left[block_number])
        right_hashes = tx_hashes(right[block_number])
        if len(left_hashes) != len(right_hashes):
            raise ValueError(f"source corpus tx count mismatch in block {block_number}")
        if all(left_hashes) and all(right_hashes) and left_hashes != right_hashes:
            raise ValueError(f"source corpus tx hash/order mismatch in block {block_number}")


def _filtered_indices(block: dict, excluded_hashes: set[str]) -> list[int]:
    hashes = tx_hashes(block)
    return [index for index, tx_hash in enumerate(hashes) if tx_hash not in excluded_hashes]


def _filter_rows(
    rows: list[tuple[set[str], set[str]]], indices: list[int]
) -> list[tuple[set[str], set[str]]]:
    return [rows[index] for index in indices]


def compute_topology_metrics(
    source_blocks: list[dict],
    native_blocks: list[dict],
    excluded_hashes: set[str] | None = None,
) -> dict:
    excluded_hashes = {value.lower() for value in (excluded_hashes or set())}
    validate_block_alignment(source_blocks, native_blocks)
    source_by_block = {int(block["block_number"]): block for block in source_blocks}
    native_by_block = {int(block["block_number"]): block for block in native_blocks}

    source_global: set[tuple[int, int, int]] = set()
    native_global: set[tuple[int, int, int]] = set()
    source_cp = native_cp = source_hot = native_hot = 0
    transactions = 0
    per_block: list[dict] = []

    for block_number in sorted(source_by_block):
        source_block = source_by_block[block_number]
        native_block = native_by_block[block_number]
        indices = _filtered_indices(source_block, excluded_hashes)
        source_rw = _filter_rows(source_rows(source_block), indices)
        native_rw = _filter_rows(native_rows(native_block, include_bank=False), indices)
        source_pairs = pairs_from_rw(source_rw)
        native_pairs = pairs_from_rw(native_rw)
        source_global.update((block_number, left, right) for left, right in source_pairs)
        native_global.update((block_number, left, right) for left, right in native_pairs)
        intersection = len(source_pairs & native_pairs)
        source_block_cp = critical_path(len(source_rw), source_pairs)
        native_block_cp = critical_path(len(native_rw), native_pairs)
        source_block_hot = hot_key_chain(source_rw)
        native_block_hot = hot_key_chain(native_rw)
        source_cp += source_block_cp
        native_cp += native_block_cp
        source_hot += source_block_hot
        native_hot += native_block_hot
        transactions += len(indices)
        per_block.append(
            {
                "block_number": block_number,
                "transactions": len(indices),
                "source_conflict_pairs": len(source_pairs),
                "native_conflict_pairs": len(native_pairs),
                "intersection_conflict_pairs": intersection,
                "precision": ratio(intersection, len(native_pairs)),
                "recall": ratio(intersection, len(source_pairs)),
                "source_critical_path": source_block_cp,
                "native_critical_path": native_block_cp,
                "source_hot_key_chain": source_block_hot,
                "native_hot_key_chain": native_block_hot,
            }
        )

    intersection = len(source_global & native_global)
    precision = ratio(intersection, len(native_global))
    recall = ratio(intersection, len(source_global))
    return {
        "blocks": len(source_by_block),
        "transactions": transactions,
        "excluded_transaction_hashes": sorted(excluded_hashes),
        "conflict_pairs": {
            "source": len(source_global),
            "native": len(native_global),
            "intersection": intersection,
            "precision": precision,
            "recall": recall,
            "f1": f1_score(precision, recall),
            "false_positive": len(native_global - source_global),
            "false_negative": len(source_global - native_global),
        },
        "critical_path": {
            "source_sum": source_cp,
            "native_sum": native_cp,
            "ratio": ratio(native_cp, source_cp),
            "relative_error": abs(native_cp - source_cp) / source_cp if source_cp else None,
        },
        "hot_key_chain": {
            "source_sum": source_hot,
            "native_sum": native_hot,
            "ratio": ratio(native_hot, source_hot),
            "relative_error": abs(native_hot - source_hot) / source_hot if source_hot else None,
        },
        "per_block": per_block,
    }


def source_conflicts_by_owner(
    block: dict,
) -> tuple[set[tuple[int, int]], dict[str, set[tuple[int, int]]]]:
    rows = source_rows(block)
    all_pairs = pairs_from_rw(rows)
    owner_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
    for left, right in all_pairs:
        for key in conflict_cause_keys(rows, left, right):
            owner = parse_source_owner(key)
            if owner is not None:
                owner_pairs[owner].add((left, right))
    return all_pairs, owner_pairs


def mapped_owner_index(catalog: dict) -> dict[str, dict]:
    result: dict[str, dict] = {}
    for item in catalog.get("instances") or []:
        owner = str(item.get("source_storage_owner") or "").lower()
        family = item.get("native_code_family")
        if not (owner.startswith("0x") and len(owner) == 42 and family):
            continue
        row = result.setdefault(
            owner,
            {
                "native_code_families": set(),
                "ethereum_profile_families": set(),
                "native_instance_ids": set(),
            },
        )
        row["native_code_families"].add(str(family))
        profile = item.get("ethereum_profile_family")
        if profile:
            row["ethereum_profile_families"].add(str(profile).lower())
        instance = item.get("native_instance_id")
        if instance:
            row["native_instance_ids"].add(str(instance))
    return result


def owner_profile_index(plan_blocks: list[dict], catalog: dict) -> dict[str, set[str]]:
    result: dict[str, set[str]] = defaultdict(set)
    for owner, item in mapped_owner_index(catalog).items():
        result[owner].update(item["ethereum_profile_families"])
    for block in plan_blocks:
        for tx in block.get("transactions") or []:
            for action in tx.get("native_actions") or []:
                owner = str(action.get("storage_context_address") or "").lower()
                profile = str(action.get("ethereum_profile_family") or "").lower()
                if owner.startswith("0x") and len(owner) == 42 and len(profile) == 64:
                    result[owner].add(profile)
    return result


def percentile(values: Iterable[float], q: float) -> float | None:
    ordered = sorted(float(value) for value in values)
    if not ordered:
        return None
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * q
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    fraction = position - low
    return ordered[low] * (1.0 - fraction) + ordered[high] * fraction


def exact_mapping_coverage(source_blocks: list[dict], catalog: dict) -> dict:
    mapped_owners = mapped_owner_index(catalog)
    total_global: set[tuple[int, int, int]] = set()
    mapped_global: set[tuple[int, int, int]] = set()
    by_family: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    by_owner: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    per_block: list[dict] = []

    for block in source_blocks:
        block_number = int(block["block_number"])
        all_pairs, owner_pairs = source_conflicts_by_owner(block)
        total_ids = {(block_number, left, right) for left, right in all_pairs}
        mapped_ids: set[tuple[int, int, int]] = set()
        total_global.update(total_ids)
        for owner, pairs in owner_pairs.items():
            if owner not in mapped_owners:
                continue
            ids = {(block_number, left, right) for left, right in pairs}
            mapped_global.update(ids)
            mapped_ids.update(ids)
            by_owner[owner].update(ids)
            for family in mapped_owners[owner]["native_code_families"]:
                by_family[family].update(ids)
        per_block.append(
            {
                "block_number": block_number,
                "total_conflict_pairs": len(total_ids),
                "mapped_owner_conflict_pairs": len(mapped_ids),
                "coverage": ratio(len(mapped_ids), len(total_ids)) if total_ids else 1.0,
            }
        )

    conflict_blocks = [row for row in per_block if row["total_conflict_pairs"] > 0]
    coverages = [float(row["coverage"]) for row in conflict_blocks]
    return {
        "definition": (
            "a source conflict pair is covered when at least one concrete EVM storage owner "
            "causing that pair has a frozen native instance mapping"
        ),
        "total_unique_conflict_pairs": len(total_global),
        "mapped_owner_unique_conflict_pairs": len(mapped_global),
        "aggregate_coverage": ratio(len(mapped_global), len(total_global)) if total_global else 1.0,
        "conflict_bearing_blocks": len(conflict_blocks),
        "median_conflict_bearing_block_coverage": statistics.median(coverages) if coverages else None,
        "p10_conflict_bearing_block_coverage": percentile(coverages, 0.10),
        "p25_conflict_bearing_block_coverage": percentile(coverages, 0.25),
        "minimum_conflict_bearing_block_coverage": min(coverages) if coverages else None,
        "by_native_family": [
            {"native_code_family": family, "unique_conflict_pairs": len(pairs)}
            for family, pairs in sorted(by_family.items(), key=lambda item: (-len(item[1]), item[0]))
        ],
        "by_source_owner": [
            {
                "source_storage_owner": owner,
                "unique_conflict_pairs": len(pairs),
                "native_code_families": sorted(mapped_owners[owner]["native_code_families"]),
                "ethereum_profile_families": sorted(mapped_owners[owner]["ethereum_profile_families"]),
            }
            for owner, pairs in sorted(by_owner.items(), key=lambda item: (-len(item[1]), item[0]))
        ],
        "per_block": per_block,
    }


def source_corpus_metrics(blocks: list[dict]) -> dict:
    pair_global: set[tuple[int, int, int]] = set()
    cp_sum = hot_sum = 0
    per_block: list[dict] = []
    for block in blocks:
        block_number = int(block["block_number"])
        rows = source_rows(block)
        pairs = pairs_from_rw(rows)
        pair_global.update((block_number, left, right) for left, right in pairs)
        cp = critical_path(len(rows), pairs)
        hot = hot_key_chain(rows)
        cp_sum += cp
        hot_sum += hot
        per_block.append(
            {
                "block_number": block_number,
                "conflict_pairs": len(pairs),
                "critical_path": cp,
                "hot_key_chain": hot,
            }
        )
    return {
        "pairs": pair_global,
        "conflict_pairs": len(pair_global),
        "critical_path_sum": cp_sum,
        "hot_key_chain_sum": hot_sum,
        "per_block": per_block,
    }


def source_trace_ablation(exact_blocks: list[dict], public_blocks: list[dict]) -> dict:
    source_source_alignment(exact_blocks, public_blocks)
    exact = source_corpus_metrics(exact_blocks)
    public = source_corpus_metrics(public_blocks)
    exact_pairs = exact.pop("pairs")
    public_pairs = public.pop("pairs")
    intersection = len(exact_pairs & public_pairs)
    exact_by_block = {row["block_number"]: row for row in exact["per_block"]}
    public_by_block = {row["block_number"]: row for row in public["per_block"]}
    expansion = []
    for block_number in sorted(exact_by_block):
        e = exact_by_block[block_number]
        p = public_by_block[block_number]
        expansion.append(
            {
                "block_number": block_number,
                "exact_conflict_pairs": e["conflict_pairs"],
                "public_conflict_pairs": p["conflict_pairs"],
                "pair_delta": e["conflict_pairs"] - p["conflict_pairs"],
                "exact_critical_path": e["critical_path"],
                "public_critical_path": p["critical_path"],
                "critical_path_delta": e["critical_path"] - p["critical_path"],
            }
        )
    expansion.sort(key=lambda row: (-row["pair_delta"], -row["critical_path_delta"], row["block_number"]))
    return {
        "exact": exact,
        "public_prestate": public,
        "intersection_conflict_pairs": intersection,
        "exact_only_conflict_pairs": len(exact_pairs - public_pairs),
        "public_only_conflict_pairs": len(public_pairs - exact_pairs),
        "public_precision_against_exact": ratio(intersection, len(public_pairs)),
        "public_recall_against_exact": ratio(intersection, len(exact_pairs)),
        "top_blocks_by_exact_pair_expansion": expansion[:25],
        "methodology_note": (
            "the exact corpus uses transaction-level SLOAD/SSTORE observations except explicitly "
            "recorded fallback exceptions; the public corpus uses prestateTracer touched storage "
            "plus diffMode state-changing writes"
        ),
    }


def _new_bucket() -> dict:
    return {
        "pair_credit": 0.0,
        "pair_incidence": 0,
        "critical_path_pair_credit": 0.0,
        "critical_path_pair_incidence": 0,
        "_blocks": set(),
    }


def _add_bucket(
    buckets: dict[str, dict],
    label: str,
    credit: float,
    block_number: int,
    critical: bool,
) -> None:
    row = buckets.setdefault(label, _new_bucket())
    row["pair_credit"] += credit
    row["pair_incidence"] += 1
    row["_blocks"].add(block_number)
    if critical:
        row["critical_path_pair_credit"] += credit
        row["critical_path_pair_incidence"] += 1


def _rank_buckets(buckets: dict[str, dict], top: int) -> list[dict]:
    rows: list[dict] = []
    for label, bucket in buckets.items():
        row = {key: value for key, value in bucket.items() if key != "_blocks"}
        row["label"] = label
        row["blocks"] = len(bucket["_blocks"])
        row["sample_blocks"] = sorted(bucket["_blocks"])[:8]
        rows.append(row)
    rows.sort(
        key=lambda row: (
            -float(row["pair_credit"]),
            -float(row["critical_path_pair_credit"]),
            -int(row["pair_incidence"]),
            str(row["label"]),
        )
    )
    return rows[:top]


def false_negative_diagnostics(
    source_blocks: list[dict],
    native_blocks: list[dict],
    catalog: dict,
    plan_blocks: list[dict],
    top: int,
) -> dict:
    validate_block_alignment(source_blocks, native_blocks)
    source_by_block = {int(block["block_number"]): block for block in source_blocks}
    native_by_block = {int(block["block_number"]): block for block in native_blocks}
    mapped = mapped_owner_index(catalog)
    profiles = owner_profile_index(plan_blocks, catalog)

    owner_buckets: dict[str, dict] = {}
    profile_buckets: dict[str, dict] = {}
    key_buckets: dict[str, dict] = {}
    class_buckets: dict[str, dict] = {}
    block_rows: list[dict] = []
    fn_total = 0
    critical_fn_total = 0
    source_critical_total = 0
    source_critical_present_native = 0

    for block_number in sorted(source_by_block):
        source_rw = source_rows(source_by_block[block_number])
        native_rw = native_rows(native_by_block[block_number], include_bank=False)
        source_pairs = pairs_from_rw(source_rw)
        native_pairs = pairs_from_rw(native_rw)
        false_negative = source_pairs - native_pairs
        source_critical = critical_path_edges(len(source_rw), source_pairs)
        critical_fn = false_negative & source_critical
        source_critical_total += len(source_critical)
        source_critical_present_native += len(source_critical & native_pairs)
        fn_total += len(false_negative)
        critical_fn_total += len(critical_fn)

        block_rows.append(
            {
                "block_number": block_number,
                "false_negative_pairs": len(false_negative),
                "critical_path_false_negative_edges": len(critical_fn),
                "source_critical_path": critical_path(len(source_rw), source_pairs),
                "native_critical_path": critical_path(len(native_rw), native_pairs),
                "critical_path_gap": critical_path(len(source_rw), source_pairs)
                - critical_path(len(native_rw), native_pairs),
            }
        )

        for left, right in false_negative:
            causes = conflict_cause_keys(source_rw, left, right)
            critical = (left, right) in source_critical
            owners = {owner for key in causes if (owner := parse_source_owner(key)) is not None}
            mapped_owners = {owner for owner in owners if owner in mapped}
            if not owners:
                pair_class = "unknown-source-key"
            elif len(mapped_owners) == len(owners):
                pair_class = "mapped-owner-semantic-gap"
            elif not mapped_owners:
                pair_class = "unmapped-source-owner"
            else:
                pair_class = "mixed-mapped-unmapped-owner"
            _add_bucket(class_buckets, pair_class, 1.0, block_number, critical)

            if not causes:
                _add_bucket(key_buckets, "unknown", 1.0, block_number, critical)
                _add_bucket(owner_buckets, "unknown", 1.0, block_number, critical)
                _add_bucket(profile_buckets, "unknown", 1.0, block_number, critical)
                continue

            per_key = 1.0 / len(causes)
            for key in sorted(causes):
                owner = parse_source_owner(key) or "unknown"
                _add_bucket(key_buckets, key, per_key, block_number, critical)
                _add_bucket(owner_buckets, owner, per_key, block_number, critical)
                owner_profiles = profiles.get(owner, set())
                if owner_profiles:
                    per_profile = per_key / len(owner_profiles)
                    for profile in sorted(owner_profiles):
                        _add_bucket(profile_buckets, profile, per_profile, block_number, critical)
                else:
                    _add_bucket(profile_buckets, "unknown", per_key, block_number, critical)

    block_rows.sort(
        key=lambda row: (
            -row["critical_path_gap"],
            -row["critical_path_false_negative_edges"],
            -row["false_negative_pairs"],
            row["block_number"],
        )
    )
    owner_rows = _rank_buckets(owner_buckets, top)
    for row in owner_rows:
        owner = row["label"]
        row["mapped_native_owner"] = owner in mapped
        row["native_code_families"] = (
            sorted(mapped[owner]["native_code_families"]) if owner in mapped else []
        )
        row["ethereum_profile_families"] = sorted(profiles.get(owner, set()))
    key_rows = _rank_buckets(key_buckets, top)
    for row in key_rows:
        row["source_storage_owner"] = parse_source_owner(row["label"])
        row["source_storage_slot"] = parse_source_slot(row["label"])
    return {
        "false_negative_pairs": fn_total,
        "false_negative_edges_on_any_source_longest_path": critical_fn_total,
        "source_longest_path_edge_count": source_critical_total,
        "source_longest_path_edges_present_in_native": source_critical_present_native,
        "source_longest_path_edge_recall": ratio(
            source_critical_present_native, source_critical_total
        ),
        "by_coverage_class": _rank_buckets(class_buckets, top),
        "by_source_owner": owner_rows,
        "by_source_profile": _rank_buckets(profile_buckets, top),
        "by_source_key": key_rows,
        "top_blocks_by_critical_path_gap": block_rows[:top],
        "classification_note": (
            "mapped-owner-semantic-gap means the source owner is already represented by a frozen "
            "native instance but the concrete native execution still misses the pair. This is a "
            "diagnostic bucket, not proof of whether the cause is incomplete state modeling, "
            "resource granularity, or intentionally different native semantics."
        ),
    }


def native_metadata_for_key(blocks: list[dict]) -> dict[str, dict]:
    metadata: dict[str, dict] = defaultdict(
        lambda: {
            "families": set(),
            "instances": set(),
            "semantic_actions": Counter(),
            "contracts": set(),
        }
    )
    for block in blocks:
        for tx in block.get("transactions") or []:
            for access in tx.get("accesses") or []:
                kind = str(access.get("kind") or "")
                if kind.startswith("bank_"):
                    continue
                key = native_identity(access)
                row = metadata[key]
                if access.get("family"):
                    row["families"].add(str(access["family"]))
                if access.get("instance_id"):
                    row["instances"].add(str(access["instance_id"]))
                if access.get("semantic_action"):
                    row["semantic_actions"][str(access["semantic_action"])] += 1
                if access.get("contract"):
                    row["contracts"].add(str(access["contract"]))
    return metadata


def hot_key_rank(
    blocks: list[dict],
    row_builder,
    top: int,
    native_metadata: dict[str, dict] | None = None,
) -> tuple[list[dict], dict[int, dict]]:
    aggregate: dict[str, dict] = defaultdict(
        lambda: {
            "touch_count_sum": 0,
            "blocks_touched": 0,
            "block_max_wins": 0,
            "block_max_contribution": 0,
            "max_single_block_touches": 0,
            "sample_blocks": [],
        }
    )
    per_block: dict[int, dict] = {}
    for block in blocks:
        block_number = int(block["block_number"])
        rows = row_builder(block)
        touched = touches_by_key(rows)
        maximum = max((len(indices) for indices in touched.values()), default=0)
        hot_keys = sorted(key for key, indices in touched.items() if len(indices) == maximum and maximum > 0)
        per_block[block_number] = {
            "block_number": block_number,
            "hot_key_chain": maximum,
            "hot_keys": hot_keys,
        }
        for key, indices in touched.items():
            count = len(indices)
            row = aggregate[key]
            row["touch_count_sum"] += count
            row["blocks_touched"] += 1
            row["max_single_block_touches"] = max(row["max_single_block_touches"], count)
            if len(row["sample_blocks"]) < 8:
                row["sample_blocks"].append(block_number)
            if count == maximum:
                row["block_max_wins"] += 1
                row["block_max_contribution"] += count

    ranked: list[dict] = []
    for key, row in aggregate.items():
        item = {"key": key, **row}
        owner = parse_source_owner(key)
        if owner is not None:
            item["source_storage_owner"] = owner
            item["source_storage_slot"] = parse_source_slot(key)
        if native_metadata is not None and key in native_metadata:
            meta = native_metadata[key]
            item["families"] = sorted(meta["families"])
            item["instances"] = sorted(meta["instances"])
            item["contracts"] = sorted(meta["contracts"])
            item["semantic_actions"] = [
                {"name": name, "access_records": count}
                for name, count in meta["semantic_actions"].most_common(8)
            ]
        ranked.append(item)
    ranked.sort(
        key=lambda row: (
            -int(row["block_max_contribution"]),
            -int(row["block_max_wins"]),
            -int(row["touch_count_sum"]),
            str(row["key"]),
        )
    )
    return ranked[:top], per_block


def hot_key_diagnostics(
    source_blocks: list[dict], native_blocks: list[dict], top: int
) -> dict:
    validate_block_alignment(source_blocks, native_blocks)
    native_meta = native_metadata_for_key(native_blocks)
    source_rank, source_per_block = hot_key_rank(source_blocks, source_rows, top)
    native_rank, native_per_block = hot_key_rank(
        native_blocks,
        lambda block: native_rows(block, include_bank=False),
        top,
        native_metadata=native_meta,
    )
    block_rows = []
    for block_number in sorted(source_per_block):
        source = source_per_block[block_number]
        native = native_per_block[block_number]
        block_rows.append(
            {
                "block_number": block_number,
                "source_hot_key_chain": source["hot_key_chain"],
                "native_hot_key_chain": native["hot_key_chain"],
                "delta": native["hot_key_chain"] - source["hot_key_chain"],
                "source_hot_keys": source["hot_keys"][:8],
                "native_hot_keys": native["hot_keys"][:8],
            }
        )
    block_rows.sort(key=lambda row: (-row["delta"], row["block_number"]))
    return {
        "source_ranked_keys": source_rank,
        "native_ranked_keys": native_rank,
        "top_blocks_by_native_minus_source_hot_key_chain": block_rows[:top],
        "ranking_note": (
            "block_max_contribution sums a key's touch count only in blocks where that key ties "
            "for the per-block maximum. Ties can therefore make ranked-key contributions overlap; "
            "the canonical hot-key-chain headline remains the sum of one maximum value per block."
        ),
    }


def fallback_hashes_from_manifest(manifest: dict) -> set[str]:
    hashes = set()
    for row in manifest.get("trace_semantics_exceptions") or []:
        tx_hash = str(row.get("tx_hash") or "").lower()
        if tx_hash.startswith("0x") and len(tx_hash) == 66:
            hashes.add(tx_hash)
    return hashes


def fallback_transaction_details(
    source_blocks: list[dict], native_blocks: list[dict], fallback_hashes: set[str]
) -> list[dict]:
    validate_block_alignment(source_blocks, native_blocks)
    native_by_block = {int(block["block_number"]): block for block in native_blocks}
    details: list[dict] = []
    for source_block in source_blocks:
        block_number = int(source_block["block_number"])
        hashes = tx_hashes(source_block)
        source_rw = source_rows(source_block)
        native_rw = native_rows(native_by_block[block_number], include_bank=False)
        source_pairs = pairs_from_rw(source_rw)
        native_pairs = pairs_from_rw(native_rw)
        source_critical = critical_path_edges(len(source_rw), source_pairs)
        native_critical = critical_path_edges(len(native_rw), native_pairs)
        source_touched = touches_by_key(source_rw)
        native_touched = touches_by_key(native_rw)
        source_hot = max((len(v) for v in source_touched.values()), default=0)
        native_hot = max((len(v) for v in native_touched.values()), default=0)
        source_hot_keys = {key for key, indices in source_touched.items() if len(indices) == source_hot}
        native_hot_keys = {key for key, indices in native_touched.items() if len(indices) == native_hot}
        for index, tx_hash in enumerate(hashes):
            if tx_hash not in fallback_hashes:
                continue
            source_incident = {pair for pair in source_pairs if index in pair}
            native_incident = {pair for pair in native_pairs if index in pair}
            source_tx_keys = source_rw[index][0] | source_rw[index][1]
            native_tx_keys = native_rw[index][0] | native_rw[index][1]
            details.append(
                {
                    "tx_hash": tx_hash,
                    "block_number": block_number,
                    "tx_index": index,
                    "source_incident_conflict_pairs": len(source_incident),
                    "native_incident_conflict_pairs": len(native_incident),
                    "source_incident_longest_path_edges": len(source_incident & source_critical),
                    "native_incident_longest_path_edges": len(native_incident & native_critical),
                    "touches_source_block_hot_key": bool(source_tx_keys & source_hot_keys),
                    "touches_native_block_hot_key": bool(native_tx_keys & native_hot_keys),
                    "source_block_hot_key_chain": source_hot,
                    "native_block_hot_key_chain": native_hot,
                }
            )
    details.sort(key=lambda row: (row["block_number"], row["tx_index"]))
    return details


def _metric_delta(without: dict, baseline: dict) -> dict:
    return {
        "source_conflict_pairs": without["conflict_pairs"]["source"]
        - baseline["conflict_pairs"]["source"],
        "native_conflict_pairs": without["conflict_pairs"]["native"]
        - baseline["conflict_pairs"]["native"],
        "intersection": without["conflict_pairs"]["intersection"]
        - baseline["conflict_pairs"]["intersection"],
        "precision_points": (
            without["conflict_pairs"]["precision"] - baseline["conflict_pairs"]["precision"]
            if without["conflict_pairs"]["precision"] is not None
            and baseline["conflict_pairs"]["precision"] is not None
            else None
        ),
        "recall_points": (
            without["conflict_pairs"]["recall"] - baseline["conflict_pairs"]["recall"]
            if without["conflict_pairs"]["recall"] is not None
            and baseline["conflict_pairs"]["recall"] is not None
            else None
        ),
        "critical_path_source": without["critical_path"]["source_sum"]
        - baseline["critical_path"]["source_sum"],
        "critical_path_native": without["critical_path"]["native_sum"]
        - baseline["critical_path"]["native_sum"],
        "hot_key_source": without["hot_key_chain"]["source_sum"]
        - baseline["hot_key_chain"]["source_sum"],
        "hot_key_native": without["hot_key_chain"]["native_sum"]
        - baseline["hot_key_chain"]["native_sum"],
    }


def finalized_semantic_gate_value(final_mapping_simulation: dict, name: str) -> float | None:
    simulation = final_mapping_simulation.get("simulation") or final_mapping_simulation
    key = {
        "semantic_transaction_coverage": "semantic_transaction_coverage",
        "semantic_call_frame_coverage": "semantic_call_frame_coverage",
    }.get(name)
    if key is None:
        return None
    value = simulation.get(key)
    return float(value) if value is not None else None


def evaluate_gates(
    mapping: dict,
    final_mapping_simulation: dict | None,
    gate_config: dict,
) -> dict:
    # Exact SLOAD/SSTORE reconstruction changes storage-conflict ground truth, not the call tree.
    # Semantic-volume gates therefore consume the selector-granular finalized mapping simulation;
    # using translation-coverage.json here would silently regress to the pre-finalization values.
    measurements = {
        "aggregate_source_conflict_coverage": mapping.get("aggregate_coverage"),
        "median_conflict_bearing_block_coverage": mapping.get(
            "median_conflict_bearing_block_coverage"
        ),
        "semantic_transaction_coverage": (
            finalized_semantic_gate_value(
                final_mapping_simulation or {}, "semantic_transaction_coverage"
            )
            if final_mapping_simulation is not None
            else None
        ),
        "semantic_call_frame_coverage": (
            finalized_semantic_gate_value(
                final_mapping_simulation or {}, "semantic_call_frame_coverage"
            )
            if final_mapping_simulation is not None
            else None
        ),
    }
    rows = []
    accepted = True
    for name, gate in (gate_config.get("gates") or {}).items():
        minimum = gate.get("minimum")
        enforced = bool(gate.get("enforced", True))
        measured = measurements.get(name)
        passed = (
            True
            if not enforced
            else measured is not None and minimum is not None and float(measured) + 1e-12 >= float(minimum)
        )
        if enforced and not passed:
            accepted = False
        rows.append(
            {
                "name": name,
                "minimum": minimum,
                "enforced": enforced,
                "measured": measured,
                "passed": passed,
            }
        )
    return {
        "accepted": accepted,
        "measurements": measurements,
        "semantic_measurement_source": "final-mapping-simulation",
        "gates": rows,
        "policy_note": gate_config.get("policy_note"),
    }


def validate_exact_manifest(manifest: dict) -> dict:
    access_semantics = str(manifest.get("access_semantics") or "")
    trace_mode = str(manifest.get("trace_mode") or "")
    exact = "sload-sstore" in access_semantics.lower() or trace_mode == "custom-js-tx"
    if not exact:
        raise ValueError(
            "exact corpus manifest does not advertise custom-js-tx / SLOAD-SSTORE semantics: "
            f"trace_mode={trace_mode!r} access_semantics={access_semantics!r}"
        )
    exceptions = manifest.get("trace_semantics_exceptions") or []
    return {
        "trace_mode": trace_mode,
        "access_semantics": access_semantics,
        "trace_semantics_exception_count": len(exceptions),
        "trace_semantics_exception_hashes": sorted(fallback_hashes_from_manifest(manifest)),
        "fallback_caveat": manifest.get("fallback_caveat"),
    }


def write_csv(path: Path, rows: list[dict], fieldnames: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames, extrasaction="ignore")
        writer.writeheader()
        for row in rows:
            writer.writerow(row)


def render_text(report: dict, top: int) -> str:
    mapping = report["exact_source_mapping_coverage"]
    gates = report["frozen_gate_recheck"]
    baseline = report["topology_baseline"]
    sensitivity = report["fallback_sensitivity"]
    fn = report["false_negative_diagnostics"]
    hot = report["hot_key_diagnostics"]
    lines = [
        "Vegeta S3 exact-ground-truth fidelity follow-up",
        "",
        "Exact corpus provenance:",
        f"  trace mode: {report['exact_manifest']['trace_mode']}",
        f"  access semantics: {report['exact_manifest']['access_semantics']}",
        f"  explicit trace exceptions: {report['exact_manifest']['trace_semantics_exception_count']}",
        "",
        "Frozen source-family coverage gates re-evaluated on exact source conflicts:",
        f"  mapped conflict pairs: {mapping['mapped_owner_unique_conflict_pairs']} / {mapping['total_unique_conflict_pairs']} ({pct(mapping['aggregate_coverage'])})",
        f"  median conflict-bearing block coverage: {pct(mapping['median_conflict_bearing_block_coverage'])}",
        f"  semantic-volume source: {gates['semantic_measurement_source']}",
    ]
    for row in gates["gates"]:
        minimum = row["minimum"]
        measured = row["measured"]
        lines.append(
            f"  gate {row['name']}: measured={pct(measured)} minimum={pct(minimum)} "
            f"{'PASS' if row['passed'] else 'FAIL'}"
            if minimum is not None
            else f"  gate {row['name']}: measured={pct(measured)} minimum=n/a {'PASS' if row['passed'] else 'FAIL'}"
        )
    lines.extend(
        [
            f"  overall frozen-gate status: {'PASS' if gates['accepted'] else 'FAIL'}",
            "",
            "Storage-to-storage topology baseline:",
            f"  source pairs: {baseline['conflict_pairs']['source']}",
            f"  native pairs: {baseline['conflict_pairs']['native']}",
            f"  intersection: {baseline['conflict_pairs']['intersection']}",
            f"  precision: {pct(baseline['conflict_pairs']['precision'])}",
            f"  recall: {pct(baseline['conflict_pairs']['recall'])}",
            f"  F1: {pct(baseline['conflict_pairs']['f1'])}",
            f"  critical-path sum: source={baseline['critical_path']['source_sum']} native={baseline['critical_path']['native_sum']} relative_error={pct(baseline['critical_path']['relative_error'])}",
            f"  hot-key-chain sum: source={baseline['hot_key_chain']['source_sum']} native={baseline['hot_key_chain']['native_sum']} relative_error={pct(baseline['hot_key_chain']['relative_error'])}",
            "",
            "Explicit fallback sensitivity (remove fallback txs and all incident pairs from BOTH graphs):",
            f"  fallback transactions: {len(sensitivity['fallback_transaction_details'])}",
            f"  source pair delta: {sensitivity['delta']['source_conflict_pairs']:+d}",
            f"  native pair delta: {sensitivity['delta']['native_conflict_pairs']:+d}",
            f"  precision delta: {pct(sensitivity['delta']['precision_points'])}",
            f"  recall delta: {pct(sensitivity['delta']['recall_points'])}",
            f"  source critical-path delta: {sensitivity['delta']['critical_path_source']:+d}",
            f"  native critical-path delta: {sensitivity['delta']['critical_path_native']:+d}",
            "",
        ]
    )
    if report.get("exact_vs_public_trace_ablation"):
        ablation = report["exact_vs_public_trace_ablation"]
        lines.extend(
            [
                "Exact SLOAD/SSTORE vs public prestateTracer source-ground-truth ablation:",
                f"  exact pairs: {ablation['exact']['conflict_pairs']}",
                f"  public pairs: {ablation['public_prestate']['conflict_pairs']}",
                f"  exact-only pairs: {ablation['exact_only_conflict_pairs']}",
                f"  public-only pairs: {ablation['public_only_conflict_pairs']}",
                f"  public recall against exact: {pct(ablation['public_recall_against_exact'])}",
                f"  exact critical-path sum: {ablation['exact']['critical_path_sum']}",
                f"  public critical-path sum: {ablation['public_prestate']['critical_path_sum']}",
                f"  exact hot-key-chain sum: {ablation['exact']['hot_key_chain_sum']}",
                f"  public hot-key-chain sum: {ablation['public_prestate']['hot_key_chain_sum']}",
                "",
            ]
        )
    lines.extend(
        [
            "False-negative diagnosis:",
            f"  false-negative pairs: {fn['false_negative_pairs']}",
            f"  FN edges on any source longest path: {fn['false_negative_edges_on_any_source_longest_path']}",
            f"  source-longest-path edge recall in native graph: {pct(fn['source_longest_path_edge_recall'])}",
            "  coverage classes:",
        ]
    )
    for row in fn["by_coverage_class"][: min(top, 8)]:
        lines.append(
            f"    {row['label']}: credit={row['pair_credit']:.2f} critical={row['critical_path_pair_credit']:.2f} blocks={row['blocks']}"
        )
    lines.append("  top FN source owners:")
    for row in fn["by_source_owner"][: min(top, 10)]:
        families = ",".join(row.get("native_code_families") or []) or "-"
        lines.append(
            f"    {row['label']}: credit={row['pair_credit']:.2f} critical={row['critical_path_pair_credit']:.2f} mapped={row['mapped_native_owner']} native={families}"
        )
    lines.extend(["", "Top hot-key diagnostics:", "  source:"])
    for row in hot["source_ranked_keys"][: min(top, 5)]:
        lines.append(
            f"    {row['key']}: block-max-contribution={row['block_max_contribution']} wins={row['block_max_wins']} owner={row.get('source_storage_owner') or '-'}"
        )
    lines.append("  native:")
    for row in hot["native_ranked_keys"][: min(top, 5)]:
        families = ",".join(row.get("families") or []) or "-"
        actions = ",".join(item["name"] for item in row.get("semantic_actions") or []) or "-"
        lines.append(
            f"    {row['key']}: block-max-contribution={row['block_max_contribution']} wins={row['block_max_wins']} family={families} actions={actions}"
        )
    lines.extend(
        [
            "",
            "Interpretation / next action:",
            "  * Do not retune the frozen coverage gates from these results.",
            "  * Inspect mapped-owner-semantic-gap FNs with critical-path credit first; patch only principled semantic/state-model errors.",
            "  * Treat unmapped long-tail FNs as coverage limitations unless they justify a separately motivated family extension.",
            "  * If frozen gates pass and no principled dominant semantic bug remains, freeze topology fidelity and proceed to scheduler/performance evaluation.",
            "  * Keep native bank-ledger dependencies separate from this storage-to-storage fidelity comparison.",
        ]
    )
    return "\n".join(lines) + "\n"


def evaluate(
    exact_corpus: Path,
    native_accesses: Path,
    native_plan: Path,
    instance_catalog: Path,
    translation_coverage_path: Path | None,
    final_mapping_simulation_path: Path,
    gates_path: Path,
    output_dir: Path,
    public_corpus: Path | None,
    exact_manifest_path: Path | None,
    top: int,
) -> dict:
    exact_blocks = read_jsonl(exact_corpus)
    native_blocks = read_jsonl(native_accesses)
    plan_blocks = read_jsonl(native_plan)
    catalog = read_json(instance_catalog)
    gate_config = read_json(gates_path)
    translation_coverage = (
        read_json(translation_coverage_path)
        if translation_coverage_path is not None and translation_coverage_path.exists()
        else None
    )
    if not final_mapping_simulation_path.exists():
        raise FileNotFoundError(
            f"finalized semantic mapping simulation is required: {final_mapping_simulation_path}"
        )
    final_mapping_simulation = read_json(final_mapping_simulation_path)
    manifest_path = exact_manifest_path or exact_corpus.parent / "manifest.json"
    exact_manifest_raw = read_json(manifest_path)
    exact_manifest = validate_exact_manifest(exact_manifest_raw)

    validate_block_alignment(exact_blocks, native_blocks)
    baseline = compute_topology_metrics(exact_blocks, native_blocks)
    mapping = exact_mapping_coverage(exact_blocks, catalog)
    gates = evaluate_gates(mapping, final_mapping_simulation, gate_config)

    fallback_hashes = fallback_hashes_from_manifest(exact_manifest_raw)
    without_fallback = compute_topology_metrics(exact_blocks, native_blocks, fallback_hashes)
    fallback_details = fallback_transaction_details(exact_blocks, native_blocks, fallback_hashes)
    fallback_sensitivity = {
        "fallback_hashes": sorted(fallback_hashes),
        "fallback_transaction_details": fallback_details,
        "without_fallback_transactions": without_fallback,
        "delta": _metric_delta(without_fallback, baseline),
        "methodology_note": (
            "sensitivity removes each explicitly non-exact fallback transaction from both source "
            "and native graphs and recomputes topology; no replacement or imputation is performed"
        ),
    }

    fn = false_negative_diagnostics(exact_blocks, native_blocks, catalog, plan_blocks, top)
    hot = hot_key_diagnostics(exact_blocks, native_blocks, top)
    ablation = None
    if public_corpus is not None and public_corpus.exists():
        ablation = source_trace_ablation(exact_blocks, read_jsonl(public_corpus))

    report = {
        "schema_version": 1,
        "dataset": "vegeta-s3-exact-fidelity-followup",
        "inputs": {
            "exact_corpus": str(exact_corpus),
            "exact_manifest": str(manifest_path),
            "public_corpus": str(public_corpus) if public_corpus is not None else None,
            "native_accesses": str(native_accesses),
            "native_plan": str(native_plan),
            "native_instance_catalog": str(instance_catalog),
            "translation_coverage": (
                str(translation_coverage_path) if translation_coverage_path is not None else None
            ),
            "final_mapping_simulation": str(final_mapping_simulation_path),
            "frozen_gate_config": str(gates_path),
        },
        "exact_manifest": exact_manifest,
        "exact_source_mapping_coverage": mapping,
        "frozen_gate_recheck": gates,
        "topology_baseline": baseline,
        "fallback_sensitivity": fallback_sensitivity,
        "exact_vs_public_trace_ablation": ablation,
        "false_negative_diagnostics": fn,
        "hot_key_diagnostics": hot,
        "methodology": {
            "post_hoc_only": True,
            "concrete_source_keys_used_for_planning_or_execution": False,
            "primary_comparison": "source EVM storage vs native CosmWasm contract storage",
            "bank_ledger": "excluded from primary fidelity and remains a separate native execution dependency domain",
            "gate_policy": "re-evaluate previously frozen thresholds; do not tune thresholds to observed exact-ground-truth performance",
        },
    }

    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "exact-fidelity-followup.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    text = render_text(report, top)
    (output_dir / "exact-fidelity-followup.txt").write_text(text, encoding="utf-8")

    fn_rows = []
    for dimension, rows in (
        ("coverage_class", fn["by_coverage_class"]),
        ("source_owner", fn["by_source_owner"]),
        ("source_profile", fn["by_source_profile"]),
        ("source_key", fn["by_source_key"]),
    ):
        for row in rows:
            fn_rows.append({"dimension": dimension, **row})
    write_csv(
        output_dir / "exact-fidelity-fn-ranking.csv",
        fn_rows,
        [
            "dimension",
            "label",
            "pair_credit",
            "pair_incidence",
            "critical_path_pair_credit",
            "critical_path_pair_incidence",
            "blocks",
            "mapped_native_owner",
            "source_storage_owner",
            "source_storage_slot",
        ],
    )

    hot_rows = []
    for domain, rows in (
        ("source", hot["source_ranked_keys"]),
        ("native", hot["native_ranked_keys"]),
    ):
        for row in rows:
            hot_rows.append(
                {
                    "domain": domain,
                    "key": row["key"],
                    "block_max_contribution": row["block_max_contribution"],
                    "block_max_wins": row["block_max_wins"],
                    "touch_count_sum": row["touch_count_sum"],
                    "blocks_touched": row["blocks_touched"],
                    "max_single_block_touches": row["max_single_block_touches"],
                    "source_storage_owner": row.get("source_storage_owner"),
                    "source_storage_slot": row.get("source_storage_slot"),
                    "families": ";".join(row.get("families") or []),
                    "instances": ";".join(row.get("instances") or []),
                    "semantic_actions": ";".join(
                        item["name"] for item in row.get("semantic_actions") or []
                    ),
                }
            )
    write_csv(
        output_dir / "exact-fidelity-hot-keys.csv",
        hot_rows,
        [
            "domain",
            "key",
            "block_max_contribution",
            "block_max_wins",
            "touch_count_sum",
            "blocks_touched",
            "max_single_block_touches",
            "source_storage_owner",
            "source_storage_slot",
            "families",
            "instances",
            "semantic_actions",
        ],
    )

    write_csv(
        output_dir / "exact-fidelity-critical-blocks.csv",
        fn["top_blocks_by_critical_path_gap"],
        [
            "block_number",
            "false_negative_pairs",
            "critical_path_false_negative_edges",
            "source_critical_path",
            "native_critical_path",
            "critical_path_gap",
        ],
    )
    write_csv(
        output_dir / "exact-fidelity-mapping-per-block.csv",
        mapping["per_block"],
        [
            "block_number",
            "total_conflict_pairs",
            "mapped_owner_conflict_pairs",
            "coverage",
        ],
    )
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exact-corpus", type=Path, default=DEFAULT_EXACT)
    parser.add_argument("--public-corpus", type=Path, default=DEFAULT_PUBLIC)
    parser.add_argument("--native-accesses", type=Path, default=DEFAULT_NATIVE)
    parser.add_argument("--native-plan", type=Path, default=DEFAULT_PLAN)
    parser.add_argument("--instance-catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument("--translation-coverage", type=Path, default=DEFAULT_TRANSLATION_COVERAGE)
    parser.add_argument(
        "--final-mapping-simulation",
        type=Path,
        default=DEFAULT_FINAL_MAPPING_SIMULATION,
        help="selector-granular finalized semantic coverage; required for semantic-volume gates",
    )
    parser.add_argument("--gates", type=Path, default=DEFAULT_GATES)
    parser.add_argument("--exact-manifest", type=Path, default=None)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--top", type=int, default=25)
    parser.add_argument(
        "--strict-gates",
        action="store_true",
        help="exit 2 if any previously frozen enforced gate fails",
    )
    parser.add_argument(
        "--no-public-ablation",
        action="store_true",
        help="skip comparison to the original public-RPC/prestateTracer corpus",
    )
    args = parser.parse_args(argv)

    try:
        report = evaluate(
            exact_corpus=args.exact_corpus,
            public_corpus=None if args.no_public_ablation else args.public_corpus,
            native_accesses=args.native_accesses,
            native_plan=args.native_plan,
            instance_catalog=args.instance_catalog,
            translation_coverage_path=args.translation_coverage,
            final_mapping_simulation_path=args.final_mapping_simulation,
            gates_path=args.gates,
            exact_manifest_path=args.exact_manifest,
            output_dir=args.output_dir,
            top=max(1, args.top),
        )
    except (FileNotFoundError, ValueError, json.JSONDecodeError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1

    text = (args.output_dir / "exact-fidelity-followup.txt").read_text(encoding="utf-8")
    print(text, end="")
    print(f"wrote {args.output_dir / 'exact-fidelity-followup.json'}")
    print(f"wrote {args.output_dir / 'exact-fidelity-followup.txt'}")
    print(f"wrote {args.output_dir / 'exact-fidelity-fn-ranking.csv'}")
    print(f"wrote {args.output_dir / 'exact-fidelity-hot-keys.csv'}")
    print(f"wrote {args.output_dir / 'exact-fidelity-critical-blocks.csv'}")
    print(f"wrote {args.output_dir / 'exact-fidelity-mapping-per-block.csv'}")

    if args.strict_gates and not report["frozen_gate_recheck"]["accepted"]:
        print("FAIL: one or more previously frozen Vegeta S3 gates failed on exact ground truth")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
