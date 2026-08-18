#!/usr/bin/env python3
"""Rank and source-review the semantic background gap in the Vegeta S3 native plan.

The native planner intentionally retains every unsupported call as ``background-fallback``.  This
script explains that remaining gap without using historical read/write sets: it groups fallback call
frames by runtime bytecode family, ranks families by marginal transaction/frame coverage, optionally
resolves representative verified source/ABI, and emits conservative fold candidates into the seven
already-frozen native archetypes.
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

from vegeta_corpus import load_blocks

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PLAN_DIR = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan"
DEFAULT_CHARACTERIZATION = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/characterization"
RETRIABLE_HTTP = {408, 425, 429, 500, 502, 503, 504}


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def normalize_address(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).lower()
    if text.startswith("0x"):
        text = text[2:]
    if len(text) != 40:
        return None
    try:
        int(text, 16)
    except ValueError:
        return None
    return "0x" + text


def normalize_runtime_code(code: Any) -> str:
    text = str(code or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if not text:
        return ""
    if len(text) % 2:
        text = "0" + text
    bytes.fromhex(text)
    return text


def runtime_family(code: Any) -> str | None:
    text = normalize_runtime_code(code)
    return hashlib.sha256(bytes.fromhex(text)).hexdigest() if text else None


def load_code_cache(path: Path) -> dict[str, dict]:
    raw = read_json(path)
    if not isinstance(raw, dict):
        raise ValueError(f"{path}: expected object")
    return {str(key).lower(): value for key, value in raw.items() if isinstance(value, dict)}


def family_for_address(address: str | None, code_cache: dict[str, dict]) -> tuple[str, str]:
    if address is None:
        return "special:no-code-address", "special"
    entry = code_cache.get(address)
    if entry is None:
        return f"unknown:{address}", "unknown-code"
    family = runtime_family(entry.get("code"))
    if family is None:
        return "special:empty-code", "empty-code"
    return family, "runtime-code"


def collect_gap(plan_blocks: list[dict], code_cache: dict[str, dict]) -> tuple[dict[str, dict], dict]:
    groups: dict[str, dict] = {}
    all_tx_ids: set[str] = set()
    semantic_tx_ids: set[str] = set()
    background_tx_ids: set[str] = set()
    total_frames = semantic_frames = fallback_frames = 0

    for block in plan_blocks:
        number = int(block["block_number"])
        for tx in block.get("transactions") or []:
            tx_id = f"{number}:{int(tx['tx_index'])}"
            all_tx_ids.add(tx_id)
            tx_class = str(tx.get("translation_class") or "")
            if tx_class == "background-only":
                background_tx_ids.add(tx_id)
            else:
                semantic_tx_ids.add(tx_id)
            for action in tx.get("native_actions") or []:
                total_frames += 1
                if action.get("translation_status") != "background-fallback":
                    semantic_frames += 1
                    continue
                fallback_frames += 1
                address = normalize_address(action.get("ethereum_code_address"))
                family, family_kind = family_for_address(address, code_cache)
                record = groups.setdefault(family, {
                    "family": family,
                    "family_kind": family_kind,
                    "frames": 0,
                    "transactions": set(),
                    "background_only_transactions": set(),
                    "addresses": Counter(),
                    "selectors": Counter(),
                    "call_types": Counter(),
                })
                record["frames"] += 1
                record["transactions"].add(tx_id)
                if tx_id in background_tx_ids:
                    record["background_only_transactions"].add(tx_id)
                if address:
                    record["addresses"][address] += 1
                record["selectors"][str(action.get("selector") or "0x").lower()] += 1
                record["call_types"][str(action.get("call_type") or "UNKNOWN").upper()] += 1

    baseline = {
        "transactions": len(all_tx_ids),
        "semantic_transactions": len(semantic_tx_ids),
        "background_only_transactions": len(background_tx_ids),
        "total_frames": total_frames,
        "semantic_frames": semantic_frames,
        "fallback_frames": fallback_frames,
        "semantic_transaction_coverage": len(semantic_tx_ids) / len(all_tx_ids) if all_tx_ids else 1.0,
        "semantic_frame_coverage": semantic_frames / total_frames if total_frames else 1.0,
    }
    return groups, baseline


def greedy_marginal_rank(groups: dict[str, dict], baseline: dict, limit: int) -> list[dict]:
    remaining = set(groups)
    covered_background: set[str] = set()
    covered_frames = 0
    ranked: list[dict] = []
    while remaining and len(ranked) < limit:
        best = None
        best_key = None
        for family in remaining:
            item = groups[family]
            marginal_bg = len(item["background_only_transactions"] - covered_background)
            key = (marginal_bg, item["frames"], len(item["transactions"]), family)
            if best_key is None or key > best_key:
                best_key = key
                best = family
        assert best is not None
        remaining.remove(best)
        item = groups[best]
        new_bg = item["background_only_transactions"] - covered_background
        covered_background.update(item["background_only_transactions"])
        covered_frames += int(item["frames"])
        representative = item["addresses"].most_common(1)[0][0] if item["addresses"] else None
        ranked.append({
            "rank": len(ranked) + 1,
            "family": best,
            "family_kind": item["family_kind"],
            "fallback_frames": int(item["frames"]),
            "transactions_with_family": len(item["transactions"]),
            "background_only_transactions_with_family": len(item["background_only_transactions"]),
            "marginal_background_transactions": len(new_bg),
            "cumulative_background_transactions_recovered": len(covered_background),
            "potential_semantic_transaction_coverage": (
                baseline["semantic_transactions"] + len(covered_background)
            ) / baseline["transactions"] if baseline["transactions"] else 1.0,
            "cumulative_fallback_frames_in_ranked_families": covered_frames,
            "potential_semantic_frame_coverage": (
                baseline["semantic_frames"] + covered_frames
            ) / baseline["total_frames"] if baseline["total_frames"] else 1.0,
            "representative_address": representative,
            "address_count": len(item["addresses"]),
            "top_addresses": [
                {"address": address, "frames": count}
                for address, count in item["addresses"].most_common(10)
            ],
            "top_selectors": [
                {"selector": selector, "frames": count}
                for selector, count in item["selectors"].most_common(12)
            ],
            "call_types": [
                {"type": call_type, "frames": count}
                for call_type, count in item["call_types"].most_common()
            ],
        })
    return ranked


def abi_signature(entry: dict) -> str | None:
    if entry.get("type") != "function" or not entry.get("name"):
        return None
    return f"{entry['name']}({','.join(str(arg.get('type', '?')) for arg in entry.get('inputs') or [])})"


def summarize_abi(abi: Any) -> dict:
    if isinstance(abi, str):
        try:
            abi = json.loads(abi)
        except json.JSONDecodeError:
            return {"function_count": 0, "function_signatures": []}
    if not isinstance(abi, list):
        return {"function_count": 0, "function_signatures": []}
    signatures = sorted({sig for entry in abi if isinstance(entry, dict) for sig in [abi_signature(entry)] if sig})
    return {"function_count": len(signatures), "function_signatures": signatures}


def summarize_sourcify(address: str, payload: dict) -> dict:
    compilation = payload.get("compilation") if isinstance(payload.get("compilation"), dict) else {}
    metadata = payload.get("metadata") if isinstance(payload.get("metadata"), dict) else {}
    identifier = compilation.get("contractIdentifier") or compilation.get("contract_identifier")
    if not identifier and isinstance(metadata.get("settings"), dict):
        target = metadata["settings"].get("compilationTarget")
        if isinstance(target, dict) and target:
            file_name, contract_name = sorted(target.items())[0]
            identifier = f"{file_name}:{contract_name}"
    return {
        "provider": "sourcify-v2",
        "status": "verified",
        "address": address,
        "contract_identifier": identifier,
        "compiler_version": compilation.get("compilerVersion") or compilation.get("compiler_version"),
        "language": compilation.get("language") or metadata.get("language"),
        "abi": summarize_abi(payload.get("abi")),
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
        "abi": summarize_abi(row.get("ABI")),
    }


class HttpJsonClient:
    def __init__(self, timeout: int = 60, retries: int = 5, backoff: float = 1.0):
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff

    def get_json(self, url: str, *, not_found_ok: bool = False) -> dict | None:
        last_error: Exception | None = None
        for attempt in range(self.retries):
            req = urllib.request.Request(url, headers={"User-Agent": "symbgraphpool-vegeta-background-gap/1.0"})
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
            print(f"source lookup failed; retrying in {delay:.1f}s: {last_error}", file=sys.stderr)
            time.sleep(delay)
        assert last_error is not None
        raise last_error


def load_source_cache(path: Path) -> dict:
    if not path.exists():
        return {"schema_version": 1, "records": {}}
    value = read_json(path)
    value.setdefault("schema_version", 1)
    value.setdefault("records", {})
    return value


def fetch_source_summary(
    address: str,
    *,
    chain_id: int,
    sourcify_base: str,
    etherscan_base: str,
    etherscan_api_key: str | None,
    client: HttpJsonClient,
) -> dict:
    payload = client.get_json(
        f"{sourcify_base.rstrip('/')}/v2/contract/{chain_id}/{address}?fields=all",
        not_found_ok=True,
    )
    if payload is not None:
        return summarize_sourcify(address, payload)
    if etherscan_api_key:
        query = urllib.parse.urlencode({
            "chainid": str(chain_id),
            "module": "contract",
            "action": "getsourcecode",
            "address": address,
            "apikey": etherscan_api_key,
        })
        payload = client.get_json(f"{etherscan_base}?{query}")
        assert payload is not None
        return summarize_etherscan(address, payload)
    return {"provider": "sourcify-v2", "status": "not-found", "address": address, "abi": {"function_count": 0, "function_signatures": []}}


def infer_fold_candidate(source_summary: dict | None) -> dict:
    if not source_summary or source_summary.get("status") != "verified":
        return {"native_fold_candidate": None, "basis": "no-verified-abi"}
    signatures = set((source_summary.get("abi") or {}).get("function_signatures") or [])
    names = {signature.split("(", 1)[0] for signature in signatures}
    if {"deposit", "withdraw", "transfer", "balanceOf"}.issubset(names):
        family = "wrapped-native-token"
    elif {"getReserves", "swap", "token0", "token1"}.issubset(names):
        family = "astroport-pair"
    elif {"ownerOf", "setApprovalForAll"}.issubset(names) and ("safeTransferFrom" in names or "transferFrom" in names):
        family = "cw721-mintable"
    elif {"claimRank", "claimMintReward"} & names:
        family = "xen-like"
    elif {"transfer", "transferFrom", "balanceOf", "approve"}.issubset(names):
        controlled = bool({"blacklist", "unBlacklist", "pause", "unpause", "configureMinter", "masterMinter"} & names)
        family = "controlled-cw20" if controlled else "cw20-base"
    elif {"swapExactTokensForTokens", "swapExactTokensForETH", "swapExactETHForTokens", "execute", "multicall"} & names:
        family = "router-helper-or-new-family"
    else:
        family = None
    return {
        "native_fold_candidate": family,
        "basis": "verified-abi-interface" if family else "verified-abi-no-existing-archetype-match",
    }


def resolve_top_sources(
    ranked: list[dict],
    *,
    limit: int,
    cache_path: Path,
    fetch: bool,
    chain_id: int,
    sourcify_base: str,
    etherscan_base: str,
    etherscan_api_key: str | None,
) -> None:
    cache = load_source_cache(cache_path)
    records = cache["records"]
    client = HttpJsonClient()
    processed = 0
    for item in ranked:
        if processed >= limit:
            break
        address = item.get("representative_address")
        if not address or item.get("family_kind") != "runtime-code":
            item["source_resolution"] = None
            item.update(infer_fold_candidate(None))
            continue
        processed += 1
        summary = (records.get(address) or {}).get("summary")
        if summary is None and fetch:
            summary = fetch_source_summary(
                address,
                chain_id=chain_id,
                sourcify_base=sourcify_base,
                etherscan_base=etherscan_base,
                etherscan_api_key=etherscan_api_key,
                client=client,
            )
            records[address] = {"summary": summary}
            write_json_atomic(cache_path, cache)
            print(f"background source [{processed}/{limit}] {address} {summary.get('status')} {summary.get('provider')}")
        item["source_resolution"] = summary
        item.update(infer_fold_candidate(summary))
    # Make fold fields explicit on records outside the source-review prefix.
    for item in ranked:
        item.setdefault("source_resolution", None)
        if "native_fold_candidate" not in item:
            item.update(infer_fold_candidate(None))


def coverage_checkpoints(ranked: list[dict], top_ns: tuple[int, ...] = (1, 5, 10, 25, 50)) -> list[dict]:
    output = []
    for n in top_ns:
        if not ranked:
            break
        item = ranked[min(n, len(ranked)) - 1]
        output.append({
            "top_n": min(n, len(ranked)),
            "potential_semantic_transaction_coverage": item["potential_semantic_transaction_coverage"],
            "potential_semantic_frame_coverage": item["potential_semantic_frame_coverage"],
            "background_transactions_recovered": item["cumulative_background_transactions_recovered"],
        })
    return output


def render_text(report: dict) -> str:
    baseline = report["baseline"]
    lines = [
        "Vegeta S3 native background-gap dossier",
        "",
        f"baseline semantic transactions: {baseline['semantic_transactions']} / {baseline['transactions']} ({baseline['semantic_transaction_coverage'] * 100:.2f}%)",
        f"baseline semantic call frames: {baseline['semantic_frames']} / {baseline['total_frames']} ({baseline['semantic_frame_coverage'] * 100:.2f}%)",
        f"background-only transactions: {baseline['background_only_transactions']}",
        f"remaining fallback frames: {baseline['fallback_frames']}",
        f"fallback runtime/special groups: {report['fallback_family_groups']}",
        "",
        "Marginal coverage checkpoints if ranked fallback families were semantically mapped:",
    ]
    for point in report["coverage_checkpoints"]:
        lines.append(
            f"  top {point['top_n']:>3}: tx={point['potential_semantic_transaction_coverage'] * 100:6.2f}% "
            f"frames={point['potential_semantic_frame_coverage'] * 100:6.2f}% "
            f"recovered_background_tx={point['background_transactions_recovered']}"
        )
    lines.extend(["", "Top fallback families:"])
    for item in report["ranked"][:25]:
        source = item.get("source_resolution") or {}
        fold = item.get("native_fold_candidate") or "-"
        lines.append(
            f"  {item['rank']:>2}. family={item['family'][:16]} kind={item['family_kind']:<12} "
            f"frames={item['fallback_frames']:<6} tx={item['transactions_with_family']:<5} "
            f"marginal_bg_tx={item['marginal_background_transactions']:<5} "
            f"source={source.get('status', '-')}/{source.get('provider', '-')} fold={fold}"
        )
        if source.get("contract_identifier"):
            lines.append(f"      source_id={source['contract_identifier']} address={item.get('representative_address')}")
        selectors = ",".join(entry["selector"] for entry in item.get("top_selectors", [])[:6])
        if selectors:
            lines.append(f"      selectors={selectors}")
    lines.extend([
        "",
        "Interpretation:",
        "  * ranking uses only call-plan metadata and runtime-code identity; no historical read/write keys are embedded;",
        "  * verified ABI folds are conservative reuse candidates, not automatic semantic equivalence claims;",
        "  * router/helper candidates remain new-family/manual-review candidates rather than being forced into the seven contract families.",
    ])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-dir", type=Path, default=DEFAULT_PLAN_DIR)
    parser.add_argument("--characterization-dir", type=Path, default=DEFAULT_CHARACTERIZATION)
    parser.add_argument("--top-families", type=int, default=100)
    parser.add_argument("--source-limit", type=int, default=25)
    parser.add_argument("--fetch-source", action="store_true")
    parser.add_argument("--chain-id", type=int, default=1)
    parser.add_argument("--sourcify-base", default="https://sourcify.dev/server")
    parser.add_argument("--etherscan-base", default="https://api.etherscan.io/v2/api")
    parser.add_argument("--etherscan-api-key", default=os.environ.get("ETHERSCAN_API_KEY"))
    args = parser.parse_args()

    plan = load_blocks(args.plan_dir / "native-plan.jsonl")
    code_cache = load_code_cache(args.characterization_dir / "code-cache.json")
    groups, baseline = collect_gap(plan, code_cache)
    ranked = greedy_marginal_rank(groups, baseline, max(1, args.top_families))
    cache_path = args.characterization_dir / "background-source-resolution-cache.json"
    resolve_top_sources(
        ranked,
        limit=max(0, args.source_limit),
        cache_path=cache_path,
        fetch=args.fetch_source,
        chain_id=args.chain_id,
        sourcify_base=args.sourcify_base,
        etherscan_base=args.etherscan_base,
        etherscan_api_key=args.etherscan_api_key,
    )
    folds = [
        {
            "rank": item["rank"],
            "family": item["family"],
            "representative_address": item["representative_address"],
            "native_fold_candidate": item["native_fold_candidate"],
            "basis": item["basis"],
            "marginal_background_transactions": item["marginal_background_transactions"],
            "fallback_frames": item["fallback_frames"],
            "source_resolution": item["source_resolution"],
        }
        for item in ranked
        if item.get("native_fold_candidate")
    ]
    report = {
        "schema_version": 1,
        "dataset": "vegeta-s3",
        "baseline": baseline,
        "fallback_family_groups": len(groups),
        "ranking_semantics": "greedy marginal background-only transaction recovery; fallback frames break ties",
        "coverage_checkpoints": coverage_checkpoints(ranked),
        "ranked": ranked,
        "verified_fold_candidates": folds,
        "source_resolution_cache": str(cache_path),
        "source_fetch_enabled": args.fetch_source,
    }
    write_json_atomic(args.plan_dir / "background-gap-dossier.json", report)
    write_json_atomic(args.plan_dir / "background-family-fold-candidates.json", {
        "schema_version": 1,
        "dataset": "vegeta-s3",
        "candidates": folds,
        "note": "verified ABI reuse candidates require manual semantic review before changing the frozen family map",
    })
    text = render_text(report)
    (args.plan_dir / "background-gap-dossier.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
