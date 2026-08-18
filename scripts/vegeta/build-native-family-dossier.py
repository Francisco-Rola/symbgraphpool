#!/usr/bin/env python3
"""Build a source/interface dossier and provisional native-family map for Vegeta S3.

The input is ``native-family-mapping-candidates.json`` produced by
``characterize-vegeta-corpus.py --native-family-mapping-candidates``.  The tool selects the
minimum number of proxy-resolved Ethereum profile families needed for a requested conflict-coverage
target (95% by default), records their observed selectors and representative storage/source
addresses, and emits a provisional CosmWasm archetype recommendation.

With ``--fetch-source``, representative source addresses are looked up in Sourcify API v2.  If a
contract is not available there and ``ETHERSCAN_API_KEY`` (or ``--etherscan-api-key``) is set, the
Etherscan API v2 ``getsourcecode`` endpoint is used as a fallback.  Results are cached atomically in
``source-resolution-cache.json`` so reruns do not repeat successful or not-found lookups.

The generated ``native-family-map.json`` is intentionally a skeleton.  Interface hints and native
archetype recommendations are triage aids, not substitutes for inspecting real source and producing
genuine LLM symbolic-analysis artifacts for the chosen native CosmWasm code families.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

DEFAULT_SOURCIFY_BASE = "https://sourcify.dev/server"
DEFAULT_ETHERSCAN_BASE = "https://api.etherscan.io/v2/api"
RETRIABLE_HTTP = {408, 425, 429, 500, 502, 503, 504}


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def normalize_address(value: str | None) -> str | None:
    if value is None:
        return None
    value = str(value).lower()
    if value.startswith("0x"):
        value = value[2:]
    if len(value) != 40:
        return None
    try:
        int(value, 16)
    except ValueError:
        return None
    return "0x" + value


def coverage_target_entry(mapping: dict, target: float) -> dict:
    entries = mapping.get("coverage_targets", [])
    if not entries:
        raise ValueError("mapping report has no coverage_targets")
    exact = [item for item in entries if abs(float(item.get("target_coverage", -1)) - target) < 1e-9]
    if not exact:
        supported = ", ".join(f"{float(item['target_coverage']) * 100:.0f}%" for item in entries)
        raise ValueError(f"target coverage {target:.4f} is not precomputed; supported targets: {supported}")
    return exact[0]


def select_profile_families(mapping: dict, target: float) -> tuple[list[dict], dict]:
    target_entry = coverage_target_entry(mapping, target)
    count = int(target_entry["minimum_profile_families"])
    ranked = mapping.get("ranked", [])
    if count <= 0 or count > len(ranked):
        raise ValueError(f"invalid minimum_profile_families={count} for {len(ranked)} ranked families")
    return ranked[:count], target_entry


def representative_source_address(item: dict) -> tuple[str | None, str | None, str | None]:
    """Return source address, storage owner, and resolution status for one profile family."""

    owners = item.get("top_storage_owners") or []
    if not owners:
        return None, None, None
    owner = owners[0]
    storage_address = normalize_address(owner.get("address"))
    implementation = normalize_address(owner.get("implementation_address"))
    status = owner.get("resolution_status")
    if implementation is not None and str(status).startswith("resolved-"):
        return implementation, storage_address, status
    return storage_address, storage_address, status


def resolution_record_for_owner(mapping: dict, storage_owner: str | None) -> dict | None:
    if storage_owner is None:
        return None
    for record in mapping.get("resolution_records", []):
        if normalize_address(record.get("storage_owner")) == storage_owner:
            return record
    return None


def delegatecall_source_candidates(mapping: dict, storage_owner: str | None) -> list[dict]:
    record = resolution_record_for_owner(mapping, storage_owner)
    if not record or record.get("resolution_status") != "observed-delegatecall-candidate":
        return []
    candidates = []
    for item in record.get("observed_delegatecall_targets") or []:
        address = normalize_address(item.get("address"))
        if address is None:
            continue
        candidates.append({
            "address": address,
            "family": item.get("family"),
            "invocations": int(item.get("invocations", 0) or 0),
        })
    candidates.sort(key=lambda item: (-item["invocations"], item["address"]))
    return candidates


def abi_signature(entry: dict) -> str | None:
    if entry.get("type") != "function" or not entry.get("name"):
        return None
    inputs = entry.get("inputs") or []
    types = [str(arg.get("type", "?")) for arg in inputs]
    return f"{entry['name']}({','.join(types)})"


def summarize_abi(abi: Any) -> dict:
    if isinstance(abi, str):
        try:
            abi = json.loads(abi)
        except json.JSONDecodeError:
            return {"function_count": 0, "function_signatures": [], "parse_error": "ABI is not JSON"}
    if not isinstance(abi, list):
        return {"function_count": 0, "function_signatures": []}
    signatures = sorted({sig for item in abi if isinstance(item, dict) for sig in [abi_signature(item)] if sig})
    return {"function_count": len(signatures), "function_signatures": signatures}


def _nested(value: Any, *path: str) -> Any:
    current = value
    for key in path:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


def summarize_sourcify(address: str, payload: dict) -> dict:
    sources = payload.get("sources") if isinstance(payload.get("sources"), dict) else {}
    source_files = sorted(sources)
    source_bytes = 0
    for record in sources.values():
        if isinstance(record, dict) and isinstance(record.get("content"), str):
            source_bytes += len(record["content"].encode("utf-8"))

    compilation = payload.get("compilation") if isinstance(payload.get("compilation"), dict) else {}
    metadata = payload.get("metadata") if isinstance(payload.get("metadata"), dict) else {}
    contract_identifier = (
        compilation.get("contractIdentifier")
        or compilation.get("contract_identifier")
        or _nested(metadata, "settings", "compilationTarget")
    )
    if isinstance(contract_identifier, dict) and contract_identifier:
        file_name, contract_name = sorted(contract_identifier.items())[0]
        contract_identifier = f"{file_name}:{contract_name}"

    compiler_version = (
        compilation.get("compilerVersion")
        or compilation.get("compiler_version")
        or _nested(metadata, "compiler", "version")
    )
    language = compilation.get("language") or metadata.get("language")
    abi_summary = summarize_abi(payload.get("abi"))

    return {
        "provider": "sourcify-v2",
        "status": "verified",
        "address": address,
        "match": payload.get("match"),
        "runtime_match": payload.get("runtimeMatch"),
        "creation_match": payload.get("creationMatch"),
        "verified_at": payload.get("verifiedAt"),
        "contract_identifier": contract_identifier,
        "language": language,
        "compiler_version": compiler_version,
        "source_file_count": len(source_files),
        "source_files": source_files,
        "source_bytes": source_bytes,
        "abi": abi_summary,
    }


def _etherscan_source_files(source_code: str) -> list[str]:
    text = source_code.strip()
    if not text:
        return []
    # Etherscan may return standard-json-like source mappings wrapped in an additional brace pair.
    candidates = [text]
    if text.startswith("{{") and text.endswith("}}"):
        candidates.append(text[1:-1])
    for candidate in candidates:
        try:
            parsed = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            sources = parsed.get("sources")
            if isinstance(sources, dict):
                return sorted(str(name) for name in sources)
            return sorted(str(name) for name in parsed if str(name).endswith((".sol", ".vy")))
    return []


def summarize_etherscan(address: str, payload: dict) -> dict:
    result = payload.get("result")
    row = result[0] if isinstance(result, list) and result and isinstance(result[0], dict) else {}
    source_code = str(row.get("SourceCode") or "")
    abi_summary = summarize_abi(row.get("ABI"))
    source_files = _etherscan_source_files(source_code)
    verified = bool(source_code) and str(row.get("ABI") or "") not in {"", "Contract source code not verified"}
    return {
        "provider": "etherscan-v2",
        "status": "verified" if verified else "not-found",
        "address": address,
        "contract_identifier": row.get("ContractName") or None,
        "language": row.get("CompilerType") or None,
        "compiler_version": row.get("CompilerVersion") or None,
        "source_file_count": len(source_files) if source_files else (1 if source_code else 0),
        "source_files": source_files,
        "source_bytes": len(source_code.encode("utf-8")),
        "abi": abi_summary,
        "proxy": row.get("Proxy") or None,
        "implementation": normalize_address(row.get("Implementation")),
        "similar_match": normalize_address(row.get("SimilarMatch")),
    }


class HttpJsonClient:
    def __init__(self, timeout: int = 60, retries: int = 5, backoff: float = 1.0):
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff

    def get_json(self, url: str, *, headers: dict[str, str] | None = None, not_found_ok: bool = False) -> dict | None:
        last_error: Exception | None = None
        for attempt in range(self.retries):
            req = urllib.request.Request(
                url,
                headers={"User-Agent": "symbgraphpool-vegeta-dossier/1.0", **(headers or {})},
            )
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
            delay = self.backoff * (2**attempt)
            print(f"HTTP lookup attempt {attempt + 1} failed; retrying in {delay:.1f}s: {last_error}", file=sys.stderr)
            time.sleep(delay)
        assert last_error is not None
        raise last_error


def fetch_sourcify(address: str, chain_id: int, base: str, client: HttpJsonClient) -> tuple[dict, dict | None]:
    url = f"{base.rstrip('/')}/v2/contract/{chain_id}/{address}?fields=all"
    payload = client.get_json(url, not_found_ok=True)
    if payload is None:
        return {"provider": "sourcify-v2", "status": "not-found", "address": address}, None
    return summarize_sourcify(address, payload), payload


def fetch_etherscan(
    address: str,
    chain_id: int,
    base: str,
    api_key: str,
    client: HttpJsonClient,
) -> tuple[dict, dict]:
    query = urllib.parse.urlencode(
        {
            "chainid": str(chain_id),
            "module": "contract",
            "action": "getsourcecode",
            "address": address,
            "apikey": api_key,
        }
    )
    payload = client.get_json(f"{base}?{query}")
    assert payload is not None
    return summarize_etherscan(address, payload), payload


def load_source_cache(path: Path) -> dict:
    if not path.exists():
        return {"schema_version": 1, "records": {}}
    value = read_json(path)
    if not isinstance(value, dict):
        raise ValueError(f"invalid source cache: {path}")
    value.setdefault("schema_version", 1)
    value.setdefault("records", {})
    return value


def resolve_sources(
    addresses: list[str],
    *,
    cache_path: Path,
    chain_id: int,
    sourcify_base: str,
    etherscan_base: str,
    etherscan_api_key: str | None,
    client: HttpJsonClient,
    delay_ms: int,
    refresh: bool,
) -> dict[str, dict]:
    cache = load_source_cache(cache_path)
    records = cache["records"]
    result: dict[str, dict] = {}

    for index, address in enumerate(addresses, start=1):
        existing = records.get(address)
        if existing is not None and not refresh:
            result[address] = existing["summary"]
            print(f"source [{index}/{len(addresses)}] reuse {address} provider={existing['summary'].get('provider')}")
            continue

        summary, raw = fetch_sourcify(address, chain_id, sourcify_base, client)
        provider_raw: dict[str, Any] = {"sourcify": raw}
        if summary["status"] != "verified" and etherscan_api_key:
            try:
                fallback, fallback_raw = fetch_etherscan(
                    address, chain_id, etherscan_base, etherscan_api_key, client
                )
                provider_raw["etherscan"] = fallback_raw
                if fallback["status"] == "verified":
                    summary = fallback
            except Exception as exc:  # preserve Sourcify not-found while exposing fallback failure
                summary = dict(summary)
                summary["etherscan_fallback_error"] = str(exc)

        records[address] = {
            "chain_id": chain_id,
            "summary": summary,
            "provider_payloads": provider_raw,
        }
        result[address] = summary
        write_json_atomic(cache_path, cache)
        print(
            f"source [{index}/{len(addresses)}] {address} "
            f"status={summary.get('status')} provider={summary.get('provider')} "
            f"files={summary.get('source_file_count', 0)}"
        )
        if delay_ms > 0 and index < len(addresses):
            time.sleep(delay_ms / 1000.0)

    return result


def archetype_recommendation(hints: list[str]) -> tuple[str, str]:
    hint_set = set(hints)
    if "constant-product-amm-pair-like" in hint_set:
        return "astroport-pair", "selector-hint"
    if "wrapped-native-token-like" in hint_set:
        return "wrapped-native-token", "selector-hint"
    if "nft-like" in hint_set:
        return "cw721-base", "selector-hint"
    if "fungible-token-like" in hint_set:
        return "cw20-base", "selector-hint"
    return "manual-review", "no-safe-interface-hint"


def build_dossier(mapping: dict, target: float, source_records: dict[str, dict] | None = None) -> dict:
    selected, target_entry = select_profile_families(mapping, target)
    source_records = source_records or {}
    families = []
    for rank, item in enumerate(selected, start=1):
        source_address, storage_owner, resolution_status = representative_source_address(item)
        hints = list(item.get("heuristic_interface_hints") or [])
        archetype, archetype_basis = archetype_recommendation(hints)
        source = source_records.get(source_address) if source_address else None
        delegate_candidates = []
        for candidate in delegatecall_source_candidates(mapping, storage_owner):
            candidate = dict(candidate)
            candidate["source_resolution"] = source_records.get(candidate["address"], {"status": "not-fetched"})
            delegate_candidates.append(candidate)
        families.append(
            {
                "rank": rank,
                "profile_family": item.get("profile_family"),
                "unique_conflict_pairs_covered": item.get("unique_conflict_pairs_covered"),
                "conflict_pair_coverage": item.get("conflict_pair_coverage"),
                "storage_owner_count": item.get("storage_owner_count"),
                "resolved_proxy_owner_count": item.get("resolved_proxy_owner_count"),
                "unresolved_delegatecall_owner_count": item.get("unresolved_delegatecall_owner_count"),
                "representative_storage_owner": storage_owner,
                "representative_source_address": source_address,
                "representative_resolution_status": resolution_status,
                "heuristic_interface_hints": hints,
                "observed_selectors": item.get("top_selectors") or [],
                "invocations": item.get("invocations"),
                "internal_invocations": item.get("internal_invocations"),
                "root_invocations": item.get("root_invocations"),
                "source_resolution": source or {"status": "not-fetched"},
                "delegatecall_source_candidates": delegate_candidates,
                "native_archetype_recommendation": archetype,
                "native_archetype_basis": archetype_basis,
            }
        )

    return {
        "schema_version": 1,
        "dataset": "vegeta-s3",
        "selection_basis": "minimum proxy-resolved profile families reaching requested unique conflict-pair coverage target",
        "target_conflict_coverage": target,
        "selected_profile_families": len(families),
        "achieved_conflict_coverage": target_entry.get("achieved_coverage"),
        "unique_conflict_pairs_at_target": target_entry.get("unique_conflict_pairs"),
        "total_unique_conflict_pairs": mapping.get("total_unique_conflict_pairs", mapping.get("unique_conflict_pairs")),
        "interface_hint_note": mapping.get("interface_hint_note"),
        "mapping_semantics": mapping.get("mapping_semantics"),
        "families": families,
    }


def build_native_family_map(dossier: dict) -> dict:
    families = []
    for item in dossier["families"]:
        source = item.get("source_resolution") or {}
        archetype = item["native_archetype_recommendation"]
        needs_manual = archetype == "manual-review"
        delegate_candidates = item.get("delegatecall_source_candidates") or []
        candidate_addresses = [candidate.get("address") for candidate in delegate_candidates if candidate.get("address")]
        families.append(
            {
                "rank": item["rank"],
                "ethereum_profile_family": item["profile_family"],
                "representative_storage_owner": item["representative_storage_owner"],
                "representative_source_address": item["representative_source_address"],
                "conflict_pair_coverage": item["conflict_pair_coverage"],
                "heuristic_interface_hints": item["heuristic_interface_hints"],
                "source_provider": source.get("provider"),
                "source_status": source.get("status"),
                "ethereum_contract_identifier": source.get("contract_identifier"),
                "native_archetype": None if needs_manual else archetype,
                "native_code_family": None,
                "native_contract_source": None,
                "symbolic_analysis": None,
                "mapping_status": "needs-manual-source-review" if needs_manual else "provisional-archetype-needs-native-source-and-llm-analysis",
                "unresolved_delegatecall_source_candidates": candidate_addresses,
                "notes": (["Generic DELEGATECALL candidate(s) require manual source/semantics review before choosing the profile implementation."] if candidate_addresses else []),
            }
        )
    return {
        "schema_version": "1-native-family-map-skeleton",
        "dataset": dossier["dataset"],
        "target_conflict_coverage": dossier["target_conflict_coverage"],
        "achieved_conflict_coverage": dossier["achieved_conflict_coverage"],
        "selected_profile_families": dossier["selected_profile_families"],
        "status": "provisional",
        "rules": [
            "Do not merge Ethereum storage-owner instances; native instances retain independent state namespaces.",
            "A shared native_code_family may serve multiple Ethereum profile families only after manual semantic review.",
            "Selector/interface hints are triage only and never replace source-derived symbolic analysis.",
            "All final native contracts require genuine LLM symbolic-analysis artifacts before production SymbGraph evaluation.",
        ],
        "families": families,
    }


def render_dossier_text(dossier: dict) -> str:
    lines = [
        "Vegeta S3 source/interface resolution dossier",
        "",
        f"target conflict coverage: {dossier['target_conflict_coverage'] * 100:.2f}%",
        f"selected profile families: {dossier['selected_profile_families']}",
        f"achieved conflict coverage: {float(dossier['achieved_conflict_coverage']) * 100:.2f}%",
        f"unique conflict pairs at target: {dossier['unique_conflict_pairs_at_target']} / {dossier['total_unique_conflict_pairs']}",
        "",
        "Selected families:",
    ]
    for item in dossier["families"]:
        source = item.get("source_resolution") or {}
        identifier = source.get("contract_identifier") or "-"
        source_status = source.get("status") or "-"
        provider = source.get("provider") or "-"
        selector_text = ",".join(
            str(sel.get("selector")) for sel in item.get("observed_selectors", [])[:6]
        ) or "-"
        lines.append(
            f"  {item['rank']:>2}. profile={item['profile_family']} "
            f"coverage={float(item['conflict_pair_coverage']) * 100:6.2f}% "
            f"archetype={item['native_archetype_recommendation']:<20} "
            f"source={source_status}/{provider} id={identifier}"
        )
        lines.append(
            f"      storage={item['representative_storage_owner']} "
            f"source_addr={item['representative_source_address']} "
            f"resolution={item['representative_resolution_status']}"
        )
        lines.append(f"      selectors={selector_text}")
        delegate_candidates = item.get("delegatecall_source_candidates") or []
        if delegate_candidates:
            rendered = ", ".join(
                f"{candidate['address']}({candidate['invocations']} calls,{candidate.get('source_resolution', {}).get('status', '-')})"
                for candidate in delegate_candidates[:3]
            )
            lines.append(f"      unresolved_delegatecall_candidates={rendered}")
    lines.extend(
        [
            "",
            "Interpretation:",
            "  * source verification and ABI metadata are evidence for manual family identification, not automatic native mapping;",
            "  * native archetype recommendations are selector-hint triage only;",
            "  * native-family-map.json remains incomplete until native source paths and genuine LLM symbolic analyses are filled in.",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mapping",
        type=Path,
        help="native-family-mapping-candidates.json",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=None,
        help="output directory (default: mapping file directory)",
    )
    parser.add_argument(
        "--target-coverage",
        type=float,
        default=0.95,
        help="precomputed conflict-coverage target from mapping report (default: 0.95)",
    )
    parser.add_argument(
        "--fetch-source",
        action="store_true",
        help="fetch verified source/ABI metadata for representative source addresses",
    )
    parser.add_argument("--chain-id", type=int, default=1)
    parser.add_argument(
        "--sourcify-base",
        default=os.environ.get("SOURCIFY_API_BASE", DEFAULT_SOURCIFY_BASE),
    )
    parser.add_argument(
        "--etherscan-base",
        default=os.environ.get("ETHERSCAN_API_BASE", DEFAULT_ETHERSCAN_BASE),
    )
    parser.add_argument(
        "--etherscan-api-key",
        default=os.environ.get("ETHERSCAN_API_KEY"),
        help="optional Etherscan V2 fallback API key (default: ETHERSCAN_API_KEY)",
    )
    parser.add_argument("--http-timeout", type=int, default=60)
    parser.add_argument("--http-retries", type=int, default=5)
    parser.add_argument("--http-backoff", type=float, default=1.0)
    parser.add_argument("--http-delay-ms", type=int, default=100)
    parser.add_argument(
        "--refresh-source-cache",
        action="store_true",
        help="refetch source metadata even if an address already exists in the cache",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not 0.0 < args.target_coverage <= 1.0:
        raise SystemExit("--target-coverage must be in (0, 1]")
    if args.chain_id <= 0:
        raise SystemExit("--chain-id must be positive")

    mapping = read_json(args.mapping)
    selected, target_entry = select_profile_families(mapping, args.target_coverage)
    output_dir = args.output_dir or args.mapping.parent
    output_dir.mkdir(parents=True, exist_ok=True)

    source_records: dict[str, dict] = {}
    if args.fetch_source:
        addresses = []
        for item in selected:
            source_address, storage_owner, _ = representative_source_address(item)
            if source_address and source_address not in addresses:
                addresses.append(source_address)
            for candidate in delegatecall_source_candidates(mapping, storage_owner):
                candidate_address = candidate["address"]
                if candidate_address not in addresses:
                    addresses.append(candidate_address)
        client = HttpJsonClient(args.http_timeout, args.http_retries, args.http_backoff)
        source_records = resolve_sources(
            addresses,
            cache_path=output_dir / "source-resolution-cache.json",
            chain_id=args.chain_id,
            sourcify_base=args.sourcify_base,
            etherscan_base=args.etherscan_base,
            etherscan_api_key=args.etherscan_api_key,
            client=client,
            delay_ms=args.http_delay_ms,
            refresh=args.refresh_source_cache,
        )

    dossier = build_dossier(mapping, args.target_coverage, source_records)
    native_map = build_native_family_map(dossier)

    dossier_json = output_dir / "native-family-dossier.json"
    dossier_text = output_dir / "native-family-dossier.txt"
    native_map_json = output_dir / "native-family-map.json"
    write_json_atomic(dossier_json, dossier)
    dossier_text.write_text(render_dossier_text(dossier), encoding="utf-8")
    write_json_atomic(native_map_json, native_map)

    print(render_dossier_text(dossier), end="")
    print(
        f"coverage target selected {int(target_entry['minimum_profile_families'])} families "
        f"for {float(target_entry['achieved_coverage']) * 100:.2f}% achieved coverage"
    )
    print(f"wrote {dossier_json}")
    print(f"wrote {dossier_text}")
    print(f"wrote {native_map_json}")
    if args.fetch_source:
        print(f"source cache: {output_dir / 'source-resolution-cache.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
