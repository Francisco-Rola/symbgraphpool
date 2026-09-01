#!/usr/bin/env python3
"""Plan high-impact native-family expansion for the Vegeta S1 workload.

This tool consumes the *frozen* S1 coverage/callTracer/code artifacts and answers the next
translation question without issuing any RPCs:

* which unmapped storage owners create the most scheduler-relevant conflicts;
* whether those owners are direct contracts or delegate/proxy storage contexts;
* which owners share the same effective implementation/runtime bytecode;
* which selectors/call types are actually exercised while that storage context is active; and
* how much *new unique conflict-pair coverage* each implementation cluster would add after
  accounting for overlap with already-mapped families and with other candidate clusters.

The output is a triage plan, not an automatic semantic mapping.  Runtime-code/selector clustering is
used only to reduce manual review effort.  Every selected cluster still requires source/semantics
review, a reviewed native implementation, and genuine symbolic-analysis artifacts before it can be
added to a publication workload.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterable

from characterize_vegeta_corpus_compat import normalize_address
from native_s3_planner_compat import FamilyResolver, load_code_cache, runtime_code_family
from vegeta_corpus import iter_blocks, storage_contract

DELEGATE_TYPES = {"DELEGATECALL", "CALLCODE"}
CALL_TYPES = {"CALL", "STATICCALL"}

# Selector-set hints intentionally mirror the characterizer's conservative triage semantics.
KNOWN_INTERFACE_SELECTORS = {
    "wrapped-native-token-like": {"0xd0e30db0", "0x2e1a7d4d"},
    "constant-product-amm-pair-like": {"0x022c0d9f", "0x0902f1ac"},
}


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def selector_from_input(value: object) -> str:
    text = str(value or "0x").lower()
    if not text.startswith("0x"):
        text = "0x" + text
    return text[:10] if len(text) >= 10 else "0x"


def interface_hints(selectors: Iterable[str]) -> list[str]:
    observed = {str(selector).lower() for selector in selectors}
    hints: list[str] = []
    if KNOWN_INTERFACE_SELECTORS["wrapped-native-token-like"].issubset(observed):
        hints.append("wrapped-native-token-like")
    if KNOWN_INTERFACE_SELECTORS["constant-product-amm-pair-like"].issubset(observed):
        hints.append("constant-product-amm-pair-like")
    if ({"0x42842e0e", "0xb88d4fde"} & observed) and "0xa22cb465" in observed:
        hints.append("nft-like")
    if {"0xa9059cbb", "0x70a08231"}.issubset(observed) and (
        {"0x23b872dd", "0x095ea7b3", "0xdd62ed3e"} & observed
    ):
        hints.append("fungible-token-like")
    if {"0xf242432a", "0x2eb2c2d6"} & observed:
        hints.append("multi-token-like")
    return hints


def walk_storage_context(frame: dict, storage_context: str | None = None, depth: int = 0):
    """Yield (frame, storage-context, code-address) with EVM delegatecall semantics."""
    if not isinstance(frame, dict):
        return
    typ = str(frame.get("type") or "CALL").upper()
    code_address = normalize_address(frame.get("to"))
    if storage_context is None:
        storage_context = code_address
    yield frame, storage_context, code_address, depth
    calls = frame.get("calls") or []
    if not isinstance(calls, list):
        return
    for child in calls:
        if not isinstance(child, dict):
            continue
        child_type = str(child.get("type") or "CALL").upper()
        child_to = normalize_address(child.get("to"))
        if child_type in DELEGATE_TYPES:
            child_context = storage_context
        elif child_type in CALL_TYPES:
            child_context = child_to
        else:
            child_context = child_to or storage_context
        yield from walk_storage_context(child, child_context, depth + 1)


class OwnerFeatures:
    def __init__(self, owner: str, pair_attributions: int, access_records: int):
        self.owner = owner
        self.pair_attributions = pair_attributions
        self.access_records = access_records
        self.invocations = 0
        self.root_context_invocations = 0
        self.internal_context_invocations = 0
        self.call_types: Counter[str] = Counter()
        self.selectors: Counter[str] = Counter()
        self.code_addresses: Counter[str] = Counter()
        self.delegate_targets: Counter[str] = Counter()
        self.tx_hashes: set[str] = set()


def load_candidate_owners(coverage: dict, limit: int) -> dict[str, OwnerFeatures]:
    result: dict[str, OwnerFeatures] = {}
    for row in (coverage.get("top_unmapped_conflict_owners") or [])[:limit]:
        owner = normalize_address(row.get("address"))
        if owner is None:
            continue
        result[owner] = OwnerFeatures(
            owner=owner,
            pair_attributions=int(row.get("owner_pair_attributions") or 0),
            access_records=int(row.get("access_records") or 0),
        )
    return result


def scan_call_cache(call_cache: Path, owners: dict[str, OwnerFeatures]) -> tuple[int, int]:
    """Populate candidate owner execution/selector/proxy evidence from frozen callTracer files."""
    blocks = frames = 0
    for index, path in enumerate(sorted(call_cache.glob("*.json"), key=lambda p: int(p.stem)), start=1):
        cached = read_json(path)
        blocks += 1
        for tx in cached.get("transactions") or []:
            tx_hash = str(tx.get("tx_hash") or "").lower()
            root = tx.get("result") or {}
            for frame, context, code_address, depth in walk_storage_context(root):
                frames += 1
                if context not in owners:
                    continue
                feature = owners[context]
                feature.invocations += 1
                if depth == 0:
                    feature.root_context_invocations += 1
                else:
                    feature.internal_context_invocations += 1
                call_type = str(frame.get("type") or "CALL").upper()
                feature.call_types[call_type] += 1
                feature.selectors[selector_from_input(frame.get("input"))] += 1
                if code_address:
                    feature.code_addresses[code_address] += 1
                if call_type in DELEGATE_TYPES and code_address and code_address != context:
                    feature.delegate_targets[code_address] += 1
                if tx_hash:
                    feature.tx_hashes.add(tx_hash)
        if index % 500 == 0:
            print(f"family-plan call-cache blocks={index} frames={frames}", flush=True)
    return blocks, frames


def runtime_family_for(code_cache: dict[str, dict], address: str | None) -> str | None:
    if address is None:
        return None
    entry = code_cache.get(address)
    return runtime_code_family(entry.get("code")) if entry else None


def representative_code_address(feature: OwnerFeatures) -> tuple[str | None, str]:
    if feature.delegate_targets:
        address, _ = sorted(feature.delegate_targets.items(), key=lambda item: (-item[1], item[0]))[0]
        return address, "delegate-target"
    if feature.code_addresses:
        # Prefer the storage owner itself when it was actually executed; otherwise use the most
        # frequently observed code address carrying this storage context.
        if feature.owner in feature.code_addresses:
            return feature.owner, "direct-owner"
        address, _ = sorted(feature.code_addresses.items(), key=lambda item: (-item[1], item[0]))[0]
        return address, "context-code-target"
    return feature.owner, "owner-fallback"


def selector_signature(selectors: Counter[str], top: int = 12) -> str:
    values = [selector for selector, _ in sorted(selectors.items(), key=lambda item: (-item[1], item[0]))[:top]]
    material = "|".join(values).encode("utf-8")
    return hashlib.sha256(material).hexdigest()[:16]


def cluster_key(feature: OwnerFeatures, code_cache: dict[str, dict]) -> tuple[str, dict]:
    rep, basis = representative_code_address(feature)
    family = runtime_family_for(code_cache, rep)
    hints = interface_hints(feature.selectors)
    selector_fp = selector_signature(feature.selectors)
    # Runtime bytecode is the strongest clustering evidence.  Selector behavior is retained in the
    # cluster metadata so reviewers can see if identical bytecode is exercised very differently.
    if family:
        key = f"runtime:{family}"
    else:
        key = f"selectors:{selector_fp}:{','.join(hints) or 'unknown'}"
    return key, {
        "representative_code_address": rep,
        "representative_code_basis": basis,
        "runtime_code_family": family,
        "selector_signature": selector_fp,
        "interface_hints": hints,
    }


def encode_pair(block_ordinal: int, left: int, right: int) -> int:
    # Ethereum blocks here are far below 2^16 transactions.  Keeping tx indexes in 16-bit lanes
    # produces compact Python integers and makes pair identity unique across the 5,000-block corpus.
    if left >= 1 << 16 or right >= 1 << 16:
        raise ValueError(f"transaction index too large for pair encoding: {left},{right}")
    return (block_ordinal << 32) | (left << 16) | right


def conflict_pairs_for_key(readers: set[int], writers: set[int]) -> set[tuple[int, int]]:
    if not writers:
        return set()
    touched = sorted(readers | writers)
    return {
        (left, right)
        for pos, left in enumerate(touched)
        for right in touched[pos + 1 :]
        if left in writers or right in writers
    }


def exact_candidate_pair_sets(
    corpus: Path,
    resolver: FamilyResolver,
    owner_to_cluster: dict[str, str],
) -> tuple[int, set[int], dict[str, set[int]], dict[str, set[int]]]:
    """Return total pairs, mapped pair ids, cluster pair ids, and owner pair ids.

    Storing ~1.2M compact integer pair ids is intentional: it lets the planner compute *exact*
    incremental coverage after overlap rather than summing owner attributions, which can double
    count the same transaction pair when it conflicts on several storage keys.
    """
    total_pairs = 0
    mapped_ids: set[int] = set()
    cluster_ids: dict[str, set[int]] = defaultdict(set)
    owner_ids: dict[str, set[int]] = defaultdict(set)
    for block_ordinal, block in enumerate(iter_blocks(corpus)):
        readers: dict[str, set[int]] = defaultdict(set)
        writers: dict[str, set[int]] = defaultdict(set)
        for tx_index, tx in enumerate(block.get("transactions") or []):
            for key in tx.get("reads") or []:
                readers[str(key)].add(tx_index)
            for key in tx.get("writes") or []:
                writers[str(key)].add(tx_index)
        all_block_pairs: set[int] = set()
        mapped_block_pairs: set[int] = set()
        for key in set(readers) | set(writers):
            pairs = conflict_pairs_for_key(readers.get(key, set()), writers.get(key, set()))
            if not pairs:
                continue
            encoded = {encode_pair(block_ordinal, left, right) for left, right in pairs}
            all_block_pairs.update(encoded)
            owner_raw = storage_contract(key)
            owner = normalize_address("0x" + owner_raw) if owner_raw else None
            if owner is None:
                continue
            _, native = resolver.native_family_for_storage_context(owner)
            if native:
                mapped_block_pairs.update(encoded)
                continue
            cluster = owner_to_cluster.get(owner)
            if cluster:
                cluster_ids[cluster].update(encoded)
                owner_ids[owner].update(encoded)
        total_pairs += len(all_block_pairs)
        mapped_ids.update(mapped_block_pairs)
        if (block_ordinal + 1) % 500 == 0:
            candidate_pairs = len(set().union(*cluster_ids.values())) if cluster_ids else 0
            print(
                f"family-plan conflict-scan blocks={block_ordinal+1} total={total_pairs} "
                f"mapped={len(mapped_ids)} candidate_union={candidate_pairs}",
                flush=True,
            )
    return total_pairs, mapped_ids, cluster_ids, owner_ids


def greedy_clusters(
    cluster_pair_ids: dict[str, set[int]], mapped_ids: set[int], total_pairs: int, target: float
) -> tuple[list[dict], set[int]]:
    covered = set(mapped_ids)
    remaining = set(cluster_pair_ids)
    ranked: list[dict] = []
    rank = 0
    while remaining and (len(covered) / total_pairs if total_pairs else 1.0) < target:
        choice = max(
            remaining,
            key=lambda cluster: (len(cluster_pair_ids[cluster] - covered), len(cluster_pair_ids[cluster]), cluster),
        )
        gain_ids = cluster_pair_ids[choice] - covered
        if not gain_ids:
            break
        rank += 1
        before = len(covered)
        covered.update(gain_ids)
        ranked.append(
            {
                "rank": rank,
                "cluster_id": choice,
                "incremental_unique_conflict_pairs": len(gain_ids),
                "raw_unique_conflict_pairs": len(cluster_pair_ids[choice]),
                "coverage_before": before / total_pairs if total_pairs else 1.0,
                "coverage_after": len(covered) / total_pairs if total_pairs else 1.0,
            }
        )
        remaining.remove(choice)
    return ranked, covered


def counter_rows(counter: Counter[str], key: str, limit: int = 12) -> list[dict]:
    return [{key: value, "count": count} for value, count in sorted(counter.items(), key=lambda item: (-item[1], item[0]))[:limit]]


def build_clusters(
    owners: dict[str, OwnerFeatures], code_cache: dict[str, dict]
) -> tuple[dict[str, dict], dict[str, str]]:
    clusters: dict[str, dict] = {}
    owner_to_cluster: dict[str, str] = {}
    for owner, feature in owners.items():
        key, meta = cluster_key(feature, code_cache)
        owner_to_cluster[owner] = key
        cluster = clusters.setdefault(
            key,
            {
                "cluster_id": key,
                **meta,
                "owners": [],
                "owner_pair_attributions": 0,
                "access_records": 0,
                "invocations": 0,
                "transactions_with_context_attribution": 0,
                "selector_counts": Counter(),
                "call_type_counts": Counter(),
                "delegate_target_counts": Counter(),
                "code_address_counts": Counter(),
            },
        )
        cluster["owners"].append(owner)
        cluster["owner_pair_attributions"] += feature.pair_attributions
        cluster["access_records"] += feature.access_records
        cluster["invocations"] += feature.invocations
        cluster["transactions_with_context_attribution"] += len(feature.tx_hashes)
        cluster["selector_counts"].update(feature.selectors)
        cluster["call_type_counts"].update(feature.call_types)
        cluster["delegate_target_counts"].update(feature.delegate_targets)
        cluster["code_address_counts"].update(feature.code_addresses)
    return clusters, owner_to_cluster


def serialize_cluster(cluster: dict, owner_ids: dict[str, set[int]]) -> dict:
    selector_counts: Counter[str] = cluster["selector_counts"]
    hints = interface_hints(selector_counts)
    owners = sorted(cluster["owners"], key=lambda owner: (-len(owner_ids.get(owner, set())), owner))
    return {
        "cluster_id": cluster["cluster_id"],
        "runtime_code_family": cluster["runtime_code_family"],
        "representative_code_address": cluster["representative_code_address"],
        "representative_code_basis": cluster["representative_code_basis"],
        "selector_signature": selector_signature(selector_counts),
        "interface_hints": hints,
        "owners": [
            {"address": owner, "unique_conflict_pairs": len(owner_ids.get(owner, set()))}
            for owner in owners
        ],
        "owner_pair_attributions": cluster["owner_pair_attributions"],
        "access_records": cluster["access_records"],
        "invocations": cluster["invocations"],
        "transactions_with_context_attribution": cluster["transactions_with_context_attribution"],
        "top_selectors": counter_rows(selector_counts, "selector"),
        "call_types": counter_rows(cluster["call_type_counts"], "type"),
        "delegate_targets": counter_rows(cluster["delegate_target_counts"], "address"),
        "code_addresses": counter_rows(cluster["code_address_counts"], "address"),
        "review_status": "needs-source-and-semantics-review",
        "native_family_recommendation": (
            "cw721-family-review" if "nft-like" in hints else
            "cw20-family-review" if "fungible-token-like" in hints else
            "wrapped-native-review" if "wrapped-native-token-like" in hints else
            "amm-family-review" if "constant-product-amm-pair-like" in hints else
            "cw1155-family-review" if "multi-token-like" in hints else
            "manual-review"
        ),
    }


def render_markdown(report: dict) -> str:
    baseline = report["baseline"]
    greedy = report["greedy_expansion"]
    clusters = {row["cluster_id"]: row for row in report["clusters"]}
    lines = [
        "# Vegeta S1 native-family expansion plan",
        "",
        "This plan is generated from the frozen S1 source conflict corpus, callTracer cache, and historical bytecode cache. It is a manual-review queue, **not** an automatic semantic mapping.",
        "",
        "## Current coverage",
        "",
        f"- Source conflict pairs: **{baseline['mapped_unique_conflict_pairs']:,}/{baseline['total_unique_conflict_pairs']:,} ({baseline['conflict_coverage']*100:.2f}%)**",
        f"- Storage-access coverage: **{baseline['storage_access_coverage']*100:.2f}%**",
        f"- Transactions touching mapped storage: **{baseline['transaction_coverage']*100:.2f}%**",
        f"- Candidate unmapped owners analyzed: **{report['candidate_owner_count']}**",
        f"- Effective implementation/code clusters: **{report['candidate_cluster_count']}**",
        "",
        "## Greedy implementation queue",
        "",
        "The gain column is exact unique conflict-pair gain after removing overlap with already mapped families and previously selected candidate clusters.",
        "",
        "| Rank | Cluster | Owners | Hints | New pairs | Projected coverage | Representative code |",
        "|---:|---|---:|---|---:|---:|---|",
    ]
    for item in greedy["selected_clusters"]:
        cluster = clusters[item["cluster_id"]]
        hints = ", ".join(cluster["interface_hints"]) or "manual"
        rep = cluster.get("representative_code_address") or "-"
        lines.append(
            f"| {item['rank']} | `{item['cluster_id'][:28]}` | {len(cluster['owners'])} | {hints} | "
            f"{item['incremental_unique_conflict_pairs']:,} | {item['coverage_after']*100:.2f}% | `{rep}` |"
        )
    if not greedy["selected_clusters"]:
        lines.append("| - | - | - | - | - | - | - |")
    lines.extend([
        "",
        f"Candidate-cluster ceiling: **{greedy['candidate_ceiling_coverage']*100:.2f}%**. "
        f"Target: **{greedy['target_coverage']*100:.2f}%**.",
        "",
    ])
    if not greedy["target_reached"]:
        lines.extend([
            "> **The current candidate set cannot reach the target by itself.** Increase `--top-owners` on a freshly generated coverage audit or implement the selected high-impact clusters first, rerun coverage, and regenerate this plan.",
            "",
        ])
    lines.extend(["## Cluster dossiers", ""])
    greedy_rank = {row["cluster_id"]: row["rank"] for row in greedy["selected_clusters"]}
    ordered = sorted(
        report["clusters"],
        key=lambda row: (greedy_rank.get(row["cluster_id"], 10**9), -row["exact_unique_conflict_pairs"], row["cluster_id"]),
    )
    for cluster in ordered:
        owners = ", ".join(f"`{row['address']}`" for row in cluster["owners"][:8])
        if len(cluster["owners"]) > 8:
            owners += f", +{len(cluster['owners'])-8} more"
        selectors = ", ".join(
            f"`{row['selector']}` ({row['count']})" for row in cluster["top_selectors"][:8]
        ) or "none observed"
        delegates = ", ".join(
            f"`{row['address']}` ({row['count']})" for row in cluster["delegate_targets"][:5]
        ) or "none"
        lines.extend([
            f"### {cluster['cluster_id']}",
            "",
            f"- Suggested review archetype: **{cluster['native_family_recommendation']}**",
            f"- Interface hints: {', '.join(cluster['interface_hints']) or 'none / manual review'}",
            f"- Exact unique conflict pairs in candidate owners: **{cluster['exact_unique_conflict_pairs']:,}**",
            f"- Owner pair attributions (can double count): {cluster['owner_pair_attributions']:,}",
            f"- Storage accesses: {cluster['access_records']:,}",
            f"- Representative effective code address: `{cluster.get('representative_code_address') or '-'}` ({cluster.get('representative_code_basis')})",
            f"- Runtime code family: `{cluster.get('runtime_code_family') or '-'}`",
            f"- Owners: {owners}",
            f"- Delegate targets: {delegates}",
            f"- Top selectors: {selectors}",
            "",
        ])
    lines.extend([
        "## Required review before adding a family",
        "",
        "1. Resolve verified source or otherwise identify the implementation semantics for the representative effective-code address.",
        "2. Confirm proxy/delegate storage semantics and whether every clustered owner can share one native **code family** while retaining independent native state instances.",
        "3. Implement only the exercised semantic entrypoints/selectors required by the frozen S1 trace; unsupported paths must remain explicit fallbacks, never silent approximations.",
        "4. Produce genuine symbolic-analysis artifacts for the new native code family.",
        "5. Rerun `run-vegeta-s1-native-coverage.sh` and regenerate this plan. Do not run the publication S1 workload until the configured coverage gates pass.",
        "",
    ])
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--coverage", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--mapping-candidates", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--call-cache", type=Path, required=True)
    ap.add_argument("--output-json", type=Path, required=True)
    ap.add_argument("--output-md", type=Path, required=True)
    ap.add_argument("--source-addresses-output", type=Path)
    ap.add_argument("--top-owners", type=int, default=50)
    ap.add_argument("--target-coverage", type=float, default=0.95)
    ns = ap.parse_args()
    if ns.top_owners <= 0:
        raise SystemExit("--top-owners must be positive")
    if not 0 < ns.target_coverage <= 1:
        raise SystemExit("--target-coverage must be in (0,1]")

    coverage = read_json(ns.coverage)
    owners = load_candidate_owners(coverage, ns.top_owners)
    if not owners:
        raise SystemExit("coverage report contains no unmapped conflict owners")
    code_cache = load_code_cache(ns.code_cache)
    mapping_candidates = read_json(ns.mapping_candidates)
    frozen_map = read_json(ns.family_map)
    resolver = FamilyResolver(frozen_map, code_cache, mapping_candidates)

    call_blocks, call_frames = scan_call_cache(ns.call_cache, owners)
    clusters, owner_to_cluster = build_clusters(owners, code_cache)
    total_pairs, mapped_ids, cluster_pair_ids, owner_pair_ids = exact_candidate_pair_sets(
        ns.corpus, resolver, owner_to_cluster
    )
    expected_total = int((coverage.get("source_conflict_coverage") or {}).get("total_unique_conflict_pairs") or 0)
    if expected_total and expected_total != total_pairs:
        raise SystemExit(
            f"conflict-pair reconstruction drift: coverage report has {expected_total}, planner reconstructed {total_pairs}"
        )
    expected_mapped = int((coverage.get("source_conflict_coverage") or {}).get("selected_family_unique_conflict_pairs") or 0)
    if expected_mapped and expected_mapped != len(mapped_ids):
        raise SystemExit(
            f"mapped-pair reconstruction drift: coverage report has {expected_mapped}, planner reconstructed {len(mapped_ids)}"
        )

    serialized_clusters = []
    for cluster_id, cluster in clusters.items():
        row = serialize_cluster(cluster, owner_pair_ids)
        row["exact_unique_conflict_pairs"] = len(cluster_pair_ids.get(cluster_id, set()))
        row["incremental_over_current_mapping"] = len(cluster_pair_ids.get(cluster_id, set()) - mapped_ids)
        serialized_clusters.append(row)
    serialized_clusters.sort(key=lambda row: (-row["incremental_over_current_mapping"], row["cluster_id"]))

    greedy, selected_covered = greedy_clusters(cluster_pair_ids, mapped_ids, total_pairs, ns.target_coverage)
    candidate_union = set(mapped_ids)
    for values in cluster_pair_ids.values():
        candidate_union.update(values)

    storage = coverage.get("storage_access_coverage") or {}
    baseline_source = coverage.get("source_conflict_coverage") or {}
    report = {
        "schema_version": 1,
        "dataset": coverage.get("dataset") or frozen_map.get("dataset") or "vegeta-s1",
        "method": "frozen callTracer/code-cache implementation clustering plus exact overlap-aware source conflict-pair greedy expansion",
        "baseline": {
            "total_unique_conflict_pairs": total_pairs,
            "mapped_unique_conflict_pairs": len(mapped_ids),
            "conflict_coverage": len(mapped_ids) / total_pairs if total_pairs else 1.0,
            "storage_access_coverage": float(storage.get("access_record_coverage") or 0.0),
            "transaction_coverage": float(storage.get("transaction_coverage") or 0.0),
        },
        "candidate_owner_count": len(owners),
        "candidate_cluster_count": len(clusters),
        "call_cache": {"blocks_scanned": call_blocks, "frames_scanned": call_frames},
        "clusters": serialized_clusters,
        "greedy_expansion": {
            "target_coverage": ns.target_coverage,
            "selected_clusters": greedy,
            "selected_cluster_count": len(greedy),
            "coverage_after_selected": len(selected_covered) / total_pairs if total_pairs else 1.0,
            "candidate_ceiling_unique_conflict_pairs": len(candidate_union),
            "candidate_ceiling_coverage": len(candidate_union) / total_pairs if total_pairs else 1.0,
            "target_reached": len(selected_covered) / total_pairs >= ns.target_coverage if total_pairs else True,
        },
        "safety_note": (
            "Code/selector clustering is triage only. No cluster becomes a native family until source/semantics, proxy storage behavior, "
            "native implementation, symbolic analysis, and rerun coverage are reviewed."
        ),
        "coverage_report_observed": {
            "source_coverage": baseline_source.get("coverage"),
            "top_unmapped_owner_rows": len(coverage.get("top_unmapped_conflict_owners") or []),
        },
    }
    write_json_atomic(ns.output_json, report)
    ns.output_md.parent.mkdir(parents=True, exist_ok=True)
    ns.output_md.write_text(render_markdown(report), encoding="utf-8")

    if ns.source_addresses_output:
        addresses: dict[str, dict] = {}
        selected_ids = {row["cluster_id"] for row in greedy}
        for cluster in serialized_clusters:
            if cluster["cluster_id"] not in selected_ids:
                continue
            for row in cluster.get("owners") or []:
                addresses.setdefault(row["address"], {"address": row["address"], "roles": []})["roles"].append("storage-owner")
            rep = cluster.get("representative_code_address")
            if rep:
                addresses.setdefault(rep, {"address": rep, "roles": []})["roles"].append("effective-code")
            for row in cluster.get("delegate_targets") or []:
                address = row.get("address")
                if address:
                    addresses.setdefault(address, {"address": address, "roles": []})["roles"].append("delegate-target")
        for row in addresses.values():
            row["roles"] = sorted(set(row["roles"]))
        write_json_atomic(ns.source_addresses_output, {
            "schema_version": 1,
            "dataset": report["dataset"],
            "purpose": "addresses requiring source/semantics review for selected S1 family-expansion clusters",
            "addresses": sorted(addresses.values(), key=lambda row: row["address"]),
        })

    print(render_markdown(report))
    print(f"wrote {ns.output_json}")
    print(f"wrote {ns.output_md}")
    if ns.source_addresses_output:
        print(f"wrote {ns.source_addresses_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
