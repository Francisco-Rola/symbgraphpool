#!/usr/bin/env python3
"""Build an evidence dossier for the S4 fifth-batch review shortlist.

The dossier is analysis-only. It never creates or applies a native-family mapping. It combines the
wide scheduler-fidelity conflict shortlist plus diagnostic access tail with exact local corpus/callTracer evidence so human review can identify
runtime families without repeated grep work:

* every observed storage owner for each shortlisted runtime family;
* direct and call-frame selectors across the frozen S4 campaign, not just top-N summaries;
* DELEGATECALL/CALLCODE targets observed while the shortlisted family owns the storage context;
* representative source transactions and storage read/write counts;
* bytecode size/hash fingerprints and canonical EIP-1167 implementation hints;
* exact planner gains plus top blocker-cluster/co-blocker context;
* any existing delegate-resolution/review-map evidence already present in the workspace.

Optional ``--fetch-source`` enriches representative storage and delegate-target addresses with
verified-contract metadata from Sourcify v2, with Etherscan v2 as an API-key-backed fallback.
Fetched metadata is triage evidence only and never changes the family map.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from native_s3_planner_compat import runtime_code_family
from vegeta_corpus import iter_blocks, storage_contract

DELEGATE_TYPES = {"DELEGATECALL", "CALLCODE"}
CALL_TYPES = {"CALL", "STATICCALL"}
RETRIABLE_HTTP = {408, 425, 429, 500, 502, 503, 504}
DEFAULT_SOURCIFY_BASE = "https://sourcify.dev/server"
DEFAULT_ETHERSCAN_BASE = "https://api.etherscan.io/v2/api"


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def normalize_address(value: Any) -> str | None:
    text = str(value or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 40:
        return None
    try:
        int(text, 16)
    except ValueError:
        return None
    return "0x" + text


def selector_from_input(value: Any) -> str:
    text = str(value or "0x").lower()
    return text[:10] if text.startswith("0x") and len(text) >= 10 else "0x"


def code_bytes(entry: Any) -> bytes:
    if not isinstance(entry, dict):
        return b""
    text = str(entry.get("code") or "")
    if text.startswith("0x"):
        text = text[2:]
    if not text:
        return b""
    try:
        return bytes.fromhex(text)
    except ValueError:
        return b""


def eip1167_implementation(code: bytes) -> str | None:
    # Canonical EIP-1167 runtime: 363d3d373d3d3d363d73<20-byte impl>5af43d82803e903d91602b57fd5bf3
    prefix = bytes.fromhex("363d3d373d3d3d363d73")
    suffix = bytes.fromhex("5af43d82803e903d91602b57fd5bf3")
    if len(code) == len(prefix) + 20 + len(suffix) and code.startswith(prefix) and code.endswith(suffix):
        return "0x" + code[len(prefix):len(prefix) + 20].hex()
    return None


def code_fingerprint(address: str, code_cache: dict[str, Any]) -> dict:
    entry = code_cache.get(address) or {}
    raw = code_bytes(entry)
    family = runtime_code_family("0x" + raw.hex()) if raw else None
    return {
        "address": address,
        "historical_code_block": entry.get("block_number"),
        "runtime_code_family": family,
        "runtime_code_bytes": len(raw),
        "runtime_code_sha256": hashlib.sha256(raw).hexdigest() if raw else None,
        "runtime_code_prefix": "0x" + raw[:32].hex() if raw else "0x",
        "canonical_eip1167_implementation": eip1167_implementation(raw),
    }


def family_for_address(address: str | None, code_cache: dict[str, Any]) -> str | None:
    if not address:
        return None
    raw = code_bytes(code_cache.get(address))
    return runtime_code_family("0x" + raw.hex()) if raw else None


def walk_calls(frame: dict, storage_context: str | None = None):
    typ = str(frame.get("type") or "CALL").upper()
    address = normalize_address(frame.get("to"))
    if storage_context is None:
        storage_context = address
    yield frame, storage_context, address
    for child in frame.get("calls") or []:
        child_typ = str(child.get("type") or "CALL").upper()
        child_to = normalize_address(child.get("to"))
        if child_typ in DELEGATE_TYPES:
            child_context = storage_context
        elif child_typ in CALL_TYPES:
            child_context = child_to
        else:
            child_context = child_to or storage_context
        yield from walk_calls(child, child_context)


def compact_counter(counter: Counter, limit: int) -> list[dict]:
    return [{"value": key, "count": int(count)} for key, count in counter.most_common(limit)]


def source_abi_summary(value: Any) -> dict:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            return {"function_count": 0, "function_signatures": [], "parse_error": "ABI is not JSON"}
    if not isinstance(value, list):
        return {"function_count": 0, "function_signatures": []}
    functions = []
    for row in value:
        if not isinstance(row, dict) or row.get("type") != "function" or not row.get("name"):
            continue
        args = ",".join(str(item.get("type", "?")) for item in row.get("inputs") or [])
        functions.append(f"{row['name']}({args})")
    return {"function_count": len(set(functions)), "function_signatures": sorted(set(functions))[:250]}


def nested(value: Any, *path: str) -> Any:
    cur = value
    for key in path:
        if not isinstance(cur, dict):
            return None
        cur = cur.get(key)
    return cur


def summarize_sourcify(address: str, payload: dict) -> dict:
    sources = payload.get("sources") if isinstance(payload.get("sources"), dict) else {}
    compilation = payload.get("compilation") if isinstance(payload.get("compilation"), dict) else {}
    metadata = payload.get("metadata") if isinstance(payload.get("metadata"), dict) else {}
    identifier = compilation.get("contractIdentifier") or compilation.get("contract_identifier") or nested(metadata, "settings", "compilationTarget")
    if isinstance(identifier, dict) and identifier:
        file_name, contract_name = sorted(identifier.items())[0]
        identifier = f"{file_name}:{contract_name}"
    return {
        "provider": "sourcify-v2",
        "status": "verified",
        "address": address,
        "contract_identifier": identifier,
        "compiler_version": compilation.get("compilerVersion") or nested(metadata, "compiler", "version"),
        "language": compilation.get("language") or metadata.get("language"),
        "source_files": sorted(sources)[:250],
        "source_file_count": len(sources),
        "abi": source_abi_summary(payload.get("abi")),
    }


def summarize_etherscan(address: str, payload: dict) -> dict:
    result = payload.get("result")
    row = result[0] if isinstance(result, list) and result and isinstance(result[0], dict) else {}
    source = str(row.get("SourceCode") or "")
    verified = bool(source) and str(row.get("ABI") or "") not in {"", "Contract source code not verified"}
    return {
        "provider": "etherscan-v2",
        "status": "verified" if verified else "not-found",
        "address": address,
        "contract_identifier": row.get("ContractName") or None,
        "compiler_version": row.get("CompilerVersion") or None,
        "language": row.get("CompilerType") or None,
        "proxy": row.get("Proxy") or None,
        "implementation": normalize_address(row.get("Implementation")),
        "similar_match": normalize_address(row.get("SimilarMatch")),
        "abi": source_abi_summary(row.get("ABI")),
    }


class HttpJsonClient:
    def __init__(self, timeout: int, retries: int, backoff: float):
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff

    def get_json(self, url: str, *, not_found_ok: bool = False) -> dict | None:
        last_error: Exception | None = None
        for attempt in range(self.retries):
            req = urllib.request.Request(url, headers={"User-Agent": "symbgraphpool-vegeta-s4-review/1.0"})
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as response:
                    return json.load(response)
            except urllib.error.HTTPError as exc:
                if exc.code == 404 and not_found_ok:
                    return None
                last_error = exc
                if exc.code not in RETRIABLE_HTTP or attempt + 1 >= self.retries:
                    raise
            except (urllib.error.URLError, TimeoutError) as exc:
                last_error = exc
                if attempt + 1 >= self.retries:
                    raise
            delay = self.backoff * (2 ** attempt)
            print(f"source lookup retry in {delay:.1f}s: {last_error}", file=sys.stderr)
            time.sleep(delay)
        assert last_error is not None
        raise last_error


def fetch_source_summary(address: str, *, chain_id: int, sourcify_base: str, etherscan_base: str, etherscan_api_key: str | None, client: HttpJsonClient) -> dict:
    url = f"{sourcify_base.rstrip('/')}/v2/contract/{chain_id}/{address}?fields=all"
    payload = client.get_json(url, not_found_ok=True)
    if payload is not None:
        return summarize_sourcify(address, payload)
    if etherscan_api_key:
        query = urllib.parse.urlencode({"chainid": str(chain_id), "module": "contract", "action": "getsourcecode", "address": address, "apikey": etherscan_api_key})
        payload = client.get_json(f"{etherscan_base}?{query}")
        assert payload is not None
        return summarize_etherscan(address, payload)
    return {"provider": "sourcify-v2", "status": "not-found", "address": address}


def load_source_cache(path: Path) -> dict:
    if not path.exists():
        return {"schema_version": 1, "records": {}}
    value = read_json(path)
    if not isinstance(value, dict):
        raise ValueError(f"invalid source cache: {path}")
    value.setdefault("schema_version", 1)
    value.setdefault("records", {})
    return value


def enrich_sources(addresses: list[str], *, cache_path: Path, refresh: bool, chain_id: int, sourcify_base: str, etherscan_base: str, etherscan_api_key: str | None, timeout: int, retries: int, backoff: float, delay_ms: int) -> dict[str, dict]:
    cache = load_source_cache(cache_path)
    records = cache["records"]
    client = HttpJsonClient(timeout, retries, backoff)
    result: dict[str, dict] = {}
    for idx, address in enumerate(addresses, start=1):
        if address in records and not refresh:
            result[address] = records[address]
            continue
        try:
            summary = fetch_source_summary(address, chain_id=chain_id, sourcify_base=sourcify_base, etherscan_base=etherscan_base, etherscan_api_key=etherscan_api_key, client=client)
        except Exception as exc:
            summary = {"status": "lookup-error", "address": address, "error": str(exc)}
        records[address] = summary
        result[address] = summary
        atomic_json(cache_path, cache)
        print(f"source [{idx}/{len(addresses)}] {address} status={summary.get('status')} provider={summary.get('provider')}")
        if delay_ms and idx < len(addresses):
            time.sleep(delay_ms / 1000.0)
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--candidates", type=Path, required=True)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--call-cache", type=Path, required=True)
    ap.add_argument("--code-cache", type=Path, required=True)
    ap.add_argument("--family-summary", type=Path, required=True)
    ap.add_argument("--selector-summary", type=Path, required=True)
    ap.add_argument("--clusters", type=Path, required=True)
    ap.add_argument("--coverage", type=Path, required=True)
    ap.add_argument("--mapping-candidates", type=Path, required=True)
    ap.add_argument("--family-map", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ap.add_argument("--representative-transactions", type=int, default=12)
    ap.add_argument("--top-selectors", type=int, default=30)
    ap.add_argument("--top-clusters-per-family", type=int, default=12)
    ap.add_argument("--top-co-blockers", type=int, default=20)
    ap.add_argument("--fetch-source", action="store_true")
    ap.add_argument("--source-cache", type=Path)
    ap.add_argument("--refresh-source-cache", action="store_true")
    ap.add_argument("--chain-id", type=int, default=1)
    ap.add_argument("--sourcify-base", default=DEFAULT_SOURCIFY_BASE)
    ap.add_argument("--etherscan-base", default=DEFAULT_ETHERSCAN_BASE)
    ap.add_argument("--etherscan-api-key", default=os.environ.get("ETHERSCAN_API_KEY"))
    ap.add_argument("--http-timeout", type=int, default=30)
    ap.add_argument("--http-retries", type=int, default=3)
    ap.add_argument("--http-backoff", type=float, default=1.0)
    ap.add_argument("--http-delay-ms", type=int, default=75)
    ns = ap.parse_args()

    for path in (ns.candidates, ns.corpus, ns.code_cache, ns.family_summary, ns.selector_summary, ns.clusters, ns.coverage, ns.mapping_candidates, ns.family_map, ns.workspace_decisions):
        if not path.exists():
            raise SystemExit(f"missing fifth-batch evidence input: {path}")
    if not ns.call_cache.is_dir():
        raise SystemExit(f"missing callTracer cache directory: {ns.call_cache}")

    candidates = read_json(ns.candidates)
    family_summary = read_json(ns.family_summary)
    selector_summary = read_json(ns.selector_summary)
    clusters = read_json(ns.clusters)
    coverage = read_json(ns.coverage)
    mapping_candidates = read_json(ns.mapping_candidates)
    family_map = read_json(ns.family_map)
    workspace = read_json(ns.workspace_decisions)
    docs = (candidates, family_summary, selector_summary, clusters, coverage, family_map, workspace)
    if any(doc.get("dataset") != "vegeta-s4" for doc in docs):
        raise SystemExit("all dataset-bearing fifth-batch evidence inputs must have dataset=vegeta-s4")

    shortlist = list(candidates.get("projected_conflict_closure_shortlist") or [])
    if not shortlist:
        raise SystemExit("fifth-batch candidate scaffold has no conflict-closure shortlist")
    target_families = [str(row.get("runtime_code_family") or "") for row in shortlist]
    if any(not family for family in target_families):
        raise SystemExit("all fifth-batch conflict candidates must have runtime_code_family")
    target_set = set(target_families)

    code_cache = {str(k).lower(): v for k, v in read_json(ns.code_cache).items() if isinstance(v, dict)}
    address_family = {address: family_for_address(address, code_cache) for address in code_cache}
    family_meta = {str(row.get("runtime_code_family") or ""): row for row in family_summary.get("runtime_families") or []}
    coverage_by_owner = {}
    for key in ("top_unmapped_conflict_owners", "top_unmapped_state_gas_owners"):
        for row in coverage.get(key) or []:
            address = normalize_address(row.get("address"))
            if address:
                coverage_by_owner.setdefault(address, {}).update(row)
    resolution_by_owner = {str(row.get("storage_owner") or "").lower(): row for row in mapping_candidates.get("resolution_records") or []}
    ambiguous_by_owner = {str(row.get("storage_owner") or "").lower(): row for row in mapping_candidates.get("ambiguous_records") or []}
    reviewed_by_family = {str(row.get("runtime_code_family") or ""): row for row in workspace.get("decisions") or [] if row.get("runtime_code_family")}
    mapped_profiles = {str(row.get("ethereum_profile_family") or ""): row for row in family_map.get("profile_mappings") or []}

    family_owners: dict[str, set[str]] = defaultdict(set)
    for family, row in family_meta.items():
        if family not in target_set:
            continue
        for item in row.get("top_addresses") or []:
            address = normalize_address(item.get("address"))
            if address:
                family_owners[family].add(address)
    for row in shortlist:
        family = str(row["runtime_code_family"])
        for raw in row.get("owner_addresses") or []:
            address = normalize_address(raw)
            if address:
                family_owners[family].add(address)
        address = normalize_address(row.get("address"))
        if address:
            family_owners[family].add(address)
    # Characterization top-N can omit owners. Recover every local code-cache address in a target family.
    for address in code_cache:
        family = address_family.get(address)
        if family in target_set:
            family_owners[family].add(address)

    direct_selectors: dict[str, Counter[str]] = defaultdict(Counter)
    frame_selectors: dict[str, Counter[str]] = defaultdict(Counter)
    storage_reads: dict[str, int] = defaultdict(int)
    storage_writes: dict[str, int] = defaultdict(int)
    storage_tx: dict[str, int] = defaultdict(int)
    storage_slots: dict[str, set[str]] = defaultdict(set)
    representative: dict[str, list[dict]] = defaultdict(list)
    delegate_targets: dict[str, Counter[tuple[str, str, str]]] = defaultdict(Counter)
    missing_call_blocks: list[int] = []
    blocks_scanned = 0
    tx_scanned = 0

    for block in iter_blocks(ns.corpus):
        blocks_scanned += 1
        bn = int(block.get("block_number", blocks_scanned))
        call_path = ns.call_cache / f"{bn}.json"
        if not call_path.exists():
            missing_call_blocks.append(bn)
            continue
        traced = read_json(call_path)
        trace_by_hash = {str(row.get("tx_hash") or "").lower(): row for row in traced.get("transactions") or []}
        for tx in block.get("transactions") or []:
            tx_scanned += 1
            tx_hash = str(tx.get("tx_hash") or "").lower()
            direct_to = normalize_address(tx.get("to"))
            direct_family = address_family.get(direct_to) if direct_to else None
            if direct_family in target_set:
                direct_selectors[direct_family][str(tx.get("selector") or selector_from_input(tx.get("input"))).lower()] += 1
                if direct_to:
                    family_owners[direct_family].add(direct_to)
            touched: dict[str, dict[str, Any]] = {}
            for kind, keys in (("read", tx.get("reads") or []), ("write", tx.get("writes") or [])):
                for key in keys:
                    raw_owner = storage_contract(str(key))
                    if not raw_owner:
                        continue
                    owner = "0x" + raw_owner
                    family = address_family.get(owner)
                    if family not in target_set:
                        continue
                    family_owners[family].add(owner)
                    slot = str(key).rsplit("/", 1)[-1]
                    storage_slots[family].add(slot)
                    rec = touched.setdefault(family, {"reads": 0, "writes": 0, "owners": set()})
                    rec["owners"].add(owner)
                    if kind == "read":
                        storage_reads[family] += 1
                        rec["reads"] += 1
                    else:
                        storage_writes[family] += 1
                        rec["writes"] += 1
            for family, rec in touched.items():
                storage_tx[family] += 1
                if len(representative[family]) < ns.representative_transactions:
                    representative[family].append({
                        "block_number": bn,
                        "tx_index": tx.get("tx_index"),
                        "tx_hash": tx.get("tx_hash"),
                        "to": normalize_address(tx.get("to")),
                        "selector": str(tx.get("selector") or selector_from_input(tx.get("input"))).lower(),
                        "gas_used": int(tx.get("gas_used", 0) or 0),
                        "family_storage_owners": sorted(rec["owners"]),
                        "family_reads": int(rec["reads"]),
                        "family_writes": int(rec["writes"]),
                    })
            traced_tx = trace_by_hash.get(tx_hash)
            if not traced_tx:
                continue
            root = traced_tx.get("result") or {}
            for frame, storage_context, address in walk_calls(root):
                typ = str(frame.get("type") or "CALL").upper()
                selector = selector_from_input(frame.get("input"))
                frame_family = address_family.get(address) if address else None
                if frame_family in target_set:
                    frame_selectors[frame_family][selector] += 1
                    family_owners[frame_family].add(address)
                if typ in DELEGATE_TYPES and storage_context:
                    storage_family = address_family.get(storage_context)
                    if storage_family in target_set and address:
                        target_family = address_family.get(address) or "unknown"
                        delegate_targets[storage_family][(address, target_family, selector)] += 1
        if blocks_scanned % 250 == 0:
            print(f"fifth-batch evidence blocks={blocks_scanned} tx={tx_scanned}", flush=True)

    if missing_call_blocks:
        sample = ", ".join(str(x) for x in missing_call_blocks[:10])
        raise SystemExit(f"fifth-batch evidence requires complete callTracer coverage; missing {len(missing_call_blocks)} blocks (sample: {sample})")

    clusters_by_family: dict[str, list[dict]] = defaultdict(list)
    co_blockers: dict[str, Counter[str]] = defaultdict(Counter)
    for cluster in clusters.get("top_clusters") or []:
        ids = [str(x) for x in cluster.get("blocker_ids") or []]
        for family in target_families:
            blocker_id = "family:" + family
            if blocker_id not in ids:
                continue
            clusters_by_family[family].append({
                "transactions": int(cluster.get("transactions", 0) or 0),
                "gas_used": int(cluster.get("gas_used", 0) or 0),
                "unmapped_access_records": int(cluster.get("unmapped_access_records", 0) or 0),
                "blocker_ids": ids,
            })
            for other in ids:
                if other != blocker_id:
                    co_blockers[family][other] += int(cluster.get("transactions", 0) or 0)

    source_addresses: set[str] = set()
    for family in target_families:
        source_addresses.update(family_owners[family])
        source_addresses.update(key[0] for key in delegate_targets[family])
    source_records: dict[str, dict] = {}
    if ns.fetch_source:
        cache_path = ns.source_cache or ns.output.with_name("s4-fifth-batch-source-resolution-cache.json")
        source_records = enrich_sources(sorted(source_addresses), cache_path=cache_path, refresh=ns.refresh_source_cache, chain_id=ns.chain_id, sourcify_base=ns.sourcify_base, etherscan_base=ns.etherscan_base, etherscan_api_key=ns.etherscan_api_key, timeout=ns.http_timeout, retries=ns.http_retries, backoff=ns.http_backoff, delay_ms=ns.http_delay_ms)

    shortlist_by_family = {str(row["runtime_code_family"]): row for row in shortlist}
    evidence_rows = []
    for priority, family in enumerate(target_families, start=1):
        candidate = shortlist_by_family[family]
        owners = sorted(family_owners[family])
        owner_rows = []
        for owner in owners:
            owner_rows.append({
                **code_fingerprint(owner, code_cache),
                "coverage_evidence": coverage_by_owner.get(owner),
                "delegate_resolution": resolution_by_owner.get(owner),
                "ambiguous_delegate_resolution": ambiguous_by_owner.get(owner),
                "verified_source": source_records.get(owner, {"status": "not-fetched"}),
            })
        delegates = []
        for (address, target_family, selector), count in delegate_targets[family].most_common(30):
            delegates.append({
                "target_address": address,
                "target_runtime_code_family": target_family,
                "selector": selector,
                "frames": int(count),
                "target_code": code_fingerprint(address, code_cache),
                "verified_source": source_records.get(address, {"status": "not-fetched"}),
            })
        cluster_rows = sorted(clusters_by_family[family], key=lambda r: (-r["gas_used"], -r["transactions"]))[:ns.top_clusters_per_family]
        local_selector_count = sum(direct_selectors[family].values()) + sum(frame_selectors[family].values())
        verified_count = sum(1 for row in owner_rows if (row.get("verified_source") or {}).get("status") == "verified") + sum(1 for row in delegates if (row.get("verified_source") or {}).get("status") == "verified")
        if verified_count:
            evidence_status = "verified-source-metadata-available"
        elif delegates:
            evidence_status = "local-delegate-target-evidence-available"
        elif local_selector_count:
            evidence_status = "local-selector-evidence-only"
        else:
            evidence_status = "insufficient-local-identity-evidence"
        evidence_rows.append({
            "priority": priority,
            "runtime_code_family": family,
            "identity_hint": candidate.get("identity_hint"),
            "suggested_native_family": candidate.get("suggested_native_family"),
            "projected_new_storage_access_records": int(candidate.get("projected_new_storage_access_records", 0) or 0),
            "projected_new_conflict_pairs": int(candidate.get("projected_new_conflict_pairs", 0) or 0),
            "projected_cumulative_conflict_coverage": float(candidate.get("projected_cumulative_conflict_coverage", 0.0) or 0.0),
            "family_summary": family_meta.get(family),
            "all_observed_storage_owners": owners,
            "owner_count": len(owners),
            "owner_evidence": owner_rows,
            "storage_activity": {
                "transactions": storage_tx[family],
                "reads": storage_reads[family],
                "writes": storage_writes[family],
                "unique_concrete_slots": len(storage_slots[family]),
            },
            "direct_selectors_exact": compact_counter(direct_selectors[family], ns.top_selectors),
            "call_frame_selectors_exact": compact_counter(frame_selectors[family], ns.top_selectors),
            "delegate_targets_exact": delegates,
            "representative_transactions": representative[family],
            "top_blocker_clusters": cluster_rows,
            "top_co_blockers": compact_counter(co_blockers[family], ns.top_co_blockers),
            "existing_workspace_decision": reviewed_by_family.get(family),
            "existing_profile_mapping": mapped_profiles.get(family),
            "evidence_status": evidence_status,
            "review_instruction": "Use this evidence to identify the contract/family and validate dependency semantics. Do not infer a native mapping from ranking, bytecode similarity, or selector shape alone.",
        })

    out = {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": "Analysis-only evidence dossier for the fifth S4 conflict-closure review shortlist; no mappings are executable from this artifact.",
        "source_scaffold": str(ns.candidates),
        "blocks_scanned": blocks_scanned,
        "transactions_scanned": tx_scanned,
        "call_cache_complete": True,
        "source_enrichment_enabled": bool(ns.fetch_source),
        "current_storage_access_coverage": float(candidates.get("current_storage_access_coverage", 0.0) or 0.0),
        "current_conflict_coverage": float(candidates.get("current_conflict_coverage", 0.0) or 0.0),
        "remaining_storage_access_deficit_records": int(candidates.get("remaining_storage_access_deficit_records", 0) or 0),
        "remaining_conflict_deficit_pairs": int(candidates.get("remaining_conflict_deficit_pairs", 0) or 0),
        "families": evidence_rows,
    }
    atomic_json(ns.output, out)

    lines = [
        "# Vegeta S4 fifth-batch evidence dossier",
        "",
        "Analysis only. No row below is an executable mapping.",
        "",
        f"Current all-storage coverage: {100*out['current_storage_access_coverage']:.2f}%",
        f"Current conflict coverage: {100*out['current_conflict_coverage']:.2f}%",
        f"Remaining all-storage deficit: {out['remaining_storage_access_deficit_records']}",
        f"Remaining conflict deficit: {out['remaining_conflict_deficit_pairs']}",
        f"Frozen blocks scanned: {blocks_scanned}",
        f"Source enrichment: {'enabled' if ns.fetch_source else 'disabled'}",
        "",
    ]
    for row in evidence_rows:
        lines += [
            f"## {row['priority']}. `{row['runtime_code_family']}`",
            f"- projected gain: conflicts +{row['projected_new_conflict_pairs']}; all-storage +{row['projected_new_storage_access_records']}; projected conflict {100*row['projected_cumulative_conflict_coverage']:.2f}%",
            f"- evidence status: {row['evidence_status']}",
            f"- owners: {row['owner_count']} ({', '.join(row['all_observed_storage_owners'][:8]) or 'none'})",
            f"- storage activity: tx={row['storage_activity']['transactions']} reads={row['storage_activity']['reads']} writes={row['storage_activity']['writes']} unique concrete slots={row['storage_activity']['unique_concrete_slots']}",
        ]
        direct = ", ".join(f"{x['value']} ({x['count']})" for x in row['direct_selectors_exact'][:12]) or "none"
        frames = ", ".join(f"{x['value']} ({x['count']})" for x in row['call_frame_selectors_exact'][:12]) or "none"
        lines += [f"- direct selectors: {direct}", f"- call-frame selectors: {frames}"]
        if row['delegate_targets_exact']:
            lines.append("- delegate targets:")
            for item in row['delegate_targets_exact'][:8]:
                source = item.get('verified_source') or {}
                ident = source.get('contract_identifier') or '-'
                lines.append(f"  - {item['target_address']} family `{item['target_runtime_code_family']}` selector={item['selector']} frames={item['frames']} source={source.get('status','not-fetched')} id={ident}")
        else:
            lines.append("- delegate targets: none observed")
        if row['top_co_blockers']:
            lines.append("- top co-blockers in retained blocker clusters: " + ", ".join(f"{x['value']} ({x['count']} tx)" for x in row['top_co_blockers'][:8]))
        for owner in row['owner_evidence'][:5]:
            source = owner.get('verified_source') or {}
            eip1167 = owner.get('canonical_eip1167_implementation') or '-'
            lines.append(f"- owner `{owner['address']}`: code_bytes={owner['runtime_code_bytes']} eip1167_impl={eip1167} source={source.get('status','not-fetched')} id={source.get('contract_identifier') or '-'}")
        if row['representative_transactions']:
            lines.append("- representative tx:")
            for tx in row['representative_transactions'][:5]:
                lines.append(f"  - block={tx['block_number']} tx={tx['tx_hash']} to={tx['to']} selector={tx['selector']} rw={tx['family_reads']}/{tx['family_writes']} gas={tx['gas_used']}")
        lines += ["", "Review conclusion: PENDING", ""]
    ns.text_output.parent.mkdir(parents=True, exist_ok=True)
    ns.text_output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {ns.output}")
    print(f"wrote {ns.text_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
