#!/usr/bin/env python3
"""Finalize selector-granular native reuse candidates for Vegeta S3 before writing Wasm.

This is the last *reconnaissance* pass. It never executes a native contract and never consumes
historical concrete read/write keys. It starts from ``native-plan.jsonl`` plus the background-gap
source dossier and:

* structurally resolves EIP-1167 / EIP-1967 implementations for high-impact background proxies;
* keeps arbitrary DELEGATECALL targets as diagnostics rather than silently treating them as proxies;
* maps only source-verified ``(runtime family, selector)`` entrypoints into existing/native-candidate
  archetypes, so a token-shaped ABI does not cause all custom application methods to become CW20;
* proposes ``cw1155-like``, ``marketplace-router``, and ``operator-filter-helper`` as additional
  candidate archetypes when verified source evidence supports them;
* diagnoses the highest-ranked still-unidentified background family;
* admits the S3 rank-1 custodial wallet only as address-scoped system/composition semantics after
  its complete observed call shapes pass a fail-closed audit (no source ABI is invented); and
* simulates semantic transaction/frame coverage under the selector rules before any Wasm is written.

Proxy storage addresses remain distinct namespaces. For shared proxy runtime bytecode, rules may be
address-scoped when different instances resolve to different implementation families.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
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
DEFAULT_GATE = ROOT / "evaluation/vegeta/s3-native-preexecution-gates.v1.json"
DEFAULT_BASE_MAP = ROOT / "evaluation/vegeta/s3-native-family-map.v1.json"

# Vegeta S3 rank-1 background runtime. The source is not verified, so this is deliberately not an
# ABI-derived contract fold. Its three observed root call shapes are independently audited below
# before any address-scoped system/composition rule is admitted.
S3_CUSTODIAL_BATCH_ADDRESS = "0xa9d1e08c7793af67e9d92fe308d5697fb81d3e43"
S3_CUSTODIAL_BATCH_FAMILY = "33986ef98393dec1eb41d737be06f5cb5fc6accf990211c3d0c6631aa130ac8a"
S3_CUSTODIAL_NATIVE_BATCH_SELECTOR = "0x1a1da075"
S3_CUSTODIAL_TOKEN_BATCH_SELECTOR = "0xca350aa6"

EIP1967_IMPLEMENTATION_SLOT = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc"
EIP1967_BEACON_SLOT = "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50"
EIP1167_RE = re.compile(r"^363d3d373d3d3d363d73([0-9a-f]{40})5af43d82803e903d91602b57fd5bf3$")
RETRIABLE_HTTP = {408, 425, 429, 500, 502, 503, 504}

# Keccak-f[1600] constants. Ethereum selectors use legacy Keccak-256 (domain byte 0x01), not
# FIPS SHA3-256. Keeping this implementation local avoids adding a Python package dependency.
_KECCAK_ROT = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
]
_KECCAK_RC = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
]
_MASK64 = (1 << 64) - 1


def _rol64(value: int, shift: int) -> int:
    if shift == 0:
        return value & _MASK64
    return ((value << shift) | (value >> (64 - shift))) & _MASK64


def _keccak_f1600(state: list[int]) -> None:
    for rc in _KECCAK_RC:
        c = [state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20] for x in range(5)]
        d = [c[(x - 1) % 5] ^ _rol64(c[(x + 1) % 5], 1) for x in range(5)]
        for y in range(5):
            for x in range(5):
                state[x + 5 * y] ^= d[x]
        b = [0] * 25
        for y in range(5):
            for x in range(5):
                b[y + 5 * ((2 * x + 3 * y) % 5)] = _rol64(state[x + 5 * y], _KECCAK_ROT[x][y])
        for y in range(5):
            for x in range(5):
                state[x + 5 * y] = b[x + 5 * y] ^ ((~b[(x + 1) % 5 + 5 * y]) & b[(x + 2) % 5 + 5 * y])
        state[0] ^= rc


def keccak256(data: bytes) -> bytes:
    rate = 136
    state = [0] * 25
    padded = bytearray(data)
    padded.append(0x01)
    while len(padded) % rate != rate - 1:
        padded.append(0)
    padded.append(0x80)
    for offset in range(0, len(padded), rate):
        block = padded[offset:offset + rate]
        for lane in range(rate // 8):
            state[lane] ^= int.from_bytes(block[lane * 8:(lane + 1) * 8], "little")
        _keccak_f1600(state)
    out = bytearray()
    while len(out) < 32:
        for lane in range(rate // 8):
            out.extend(state[lane].to_bytes(8, "little"))
        if len(out) < 32:
            _keccak_f1600(state)
    return bytes(out[:32])


def selector_for_signature(signature: str) -> str:
    return "0x" + keccak256(signature.encode("utf-8"))[:4].hex()


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


def int_value(value: Any) -> int:
    if value is None:
        return 0
    if isinstance(value, int):
        return value
    text = str(value).strip().lower()
    if not text:
        return 0
    return int(text, 16) if text.startswith("0x") else int(text)


def normalize_runtime_code(value: Any) -> str:
    text = str(value or "").lower()
    if text.startswith("0x"):
        text = text[2:]
    if not text:
        return ""
    if len(text) % 2:
        text = "0" + text
    bytes.fromhex(text)
    return text


def runtime_family(value: Any) -> str | None:
    code = normalize_runtime_code(value)
    return hashlib.sha256(bytes.fromhex(code)).hexdigest() if code else None


def detect_eip1167(value: Any) -> str | None:
    match = EIP1167_RE.match(normalize_runtime_code(value))
    return normalize_address(match.group(1)) if match else None


def load_code_cache(path: Path) -> dict[str, dict]:
    value = read_json(path)
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected object")
    return {str(address).lower(): row for address, row in value.items() if isinstance(row, dict)}


def family_for_address(address: str | None, code_cache: dict[str, dict]) -> str | None:
    if address is None:
        return None
    row = code_cache.get(address)
    return runtime_family((row or {}).get("code")) if row else None


def _address_from_storage_word(value: Any) -> str | None:
    raw = str(value or "0x").lower()
    if raw.startswith("0x"):
        raw = raw[2:]
    if not raw or len(raw) > 64:
        return None
    try:
        numeric = int(raw, 16)
    except ValueError:
        return None
    if numeric == 0:
        return None
    return normalize_address(raw.rjust(64, "0")[-40:])


class RpcClient:
    def __init__(self, url: str, timeout: int = 60, retries: int = 5, backoff: float = 1.0):
        self.url = url
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff
        self.request_id = 0

    def call(self, method: str, params: list[Any]) -> Any:
        last_error: Exception | None = None
        for attempt in range(self.retries):
            self.request_id += 1
            payload = json.dumps({"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}).encode()
            request = urllib.request.Request(self.url, data=payload, headers={"Content-Type": "application/json", "User-Agent": "symbgraphpool-vegeta-finalizer/1.0"})
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    body = json.load(response)
                if body.get("error"):
                    raise RuntimeError(f"RPC {method}: {body['error']}")
                return body.get("result")
            except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, RuntimeError) as exc:
                last_error = exc
                retriable = not isinstance(exc, urllib.error.HTTPError) or exc.code in RETRIABLE_HTTP
                if not retriable or attempt + 1 >= self.retries:
                    raise
                delay = self.backoff * (2**attempt)
                print(f"RPC {method} failed; retrying in {delay:.1f}s: {exc}", file=sys.stderr)
                time.sleep(delay)
        assert last_error is not None
        raise last_error


class HttpJsonClient:
    def __init__(self, timeout: int = 60, retries: int = 5, backoff: float = 1.0):
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff

    def get_json(self, url: str, *, not_found_ok: bool = False) -> dict | None:
        for attempt in range(self.retries):
            request = urllib.request.Request(url, headers={"User-Agent": "symbgraphpool-vegeta-finalizer/1.0"})
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    return json.load(response)
            except urllib.error.HTTPError as exc:
                if exc.code == 404 and not_found_ok:
                    return None
                if exc.code not in RETRIABLE_HTTP or attempt + 1 >= self.retries:
                    raise
            except (urllib.error.URLError, TimeoutError):
                if attempt + 1 >= self.retries:
                    raise
            time.sleep(self.backoff * (2**attempt))
        return None


def abi_signature(entry: dict) -> str | None:
    if entry.get("type") != "function" or not entry.get("name"):
        return None
    return f"{entry['name']}({','.join(str(arg.get('type', '?')) for arg in entry.get('inputs') or [])})"


def summarize_abi(abi: Any) -> dict:
    if isinstance(abi, str):
        try:
            abi = json.loads(abi)
        except json.JSONDecodeError:
            abi = []
    if not isinstance(abi, list):
        abi = []
    signatures = sorted({sig for entry in abi if isinstance(entry, dict) for sig in [abi_signature(entry)] if sig})
    return {
        "function_count": len(signatures),
        "function_signatures": signatures,
        "selector_to_signatures": selector_index(signatures),
    }


def selector_index(signatures: list[str]) -> dict[str, list[str]]:
    index: dict[str, list[str]] = defaultdict(list)
    for signature in signatures:
        index[selector_for_signature(signature)].append(signature)
    return {selector: sorted(values) for selector, values in sorted(index.items())}


def normalize_source_summary(summary: dict | None) -> dict | None:
    if not summary:
        return None
    output = dict(summary)
    abi = dict(output.get("abi") or {})
    signatures = list(abi.get("function_signatures") or [])
    abi["selector_to_signatures"] = selector_index(signatures)
    abi["function_count"] = len(signatures)
    output["abi"] = abi
    return output


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
    rows = payload.get("result")
    row = rows[0] if isinstance(rows, list) and rows and isinstance(rows[0], dict) else {}
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


def fetch_source(address: str, *, chain_id: int, sourcify_base: str, etherscan_base: str, etherscan_key: str | None, client: HttpJsonClient) -> dict:
    payload = client.get_json(f"{sourcify_base.rstrip('/')}/v2/contract/{chain_id}/{address}?fields=all", not_found_ok=True)
    if payload is not None:
        return summarize_sourcify(address, payload)
    if etherscan_key:
        query = urllib.parse.urlencode({"chainid": str(chain_id), "module": "contract", "action": "getsourcecode", "address": address, "apikey": etherscan_key})
        payload = client.get_json(f"{etherscan_base}?{query}")
        assert payload is not None
        return summarize_etherscan(address, payload)
    return {"provider": "sourcify-v2", "status": "not-found", "address": address, "abi": {"function_count": 0, "function_signatures": [], "selector_to_signatures": {}}}


def load_summary_caches(characterization_dir: Path) -> dict[str, dict]:
    summaries: dict[str, dict] = {}
    for name in ["background-source-resolution-cache.json", "source-resolution-cache.json", "background-finalization-source-cache.json"]:
        path = characterization_dir / name
        if not path.exists():
            continue
        value = read_json(path)
        records = value.get("records", {}) if isinstance(value, dict) and "records" in value else value
        if not isinstance(records, dict):
            continue
        for address, record in records.items():
            address = normalize_address(address)
            if address is None or not isinstance(record, dict):
                continue
            summary = record.get("summary") if isinstance(record.get("summary"), dict) else record
            normalized = normalize_source_summary(summary)
            if normalized:
                summaries[address] = normalized
    return summaries


def persist_final_source_summary(characterization_dir: Path, address: str, summary: dict) -> None:
    path = characterization_dir / "background-finalization-source-cache.json"
    value = read_json(path) if path.exists() else {"schema_version": 1, "records": {}}
    value.setdefault("schema_version", 1)
    value.setdefault("records", {})[address] = {"summary": summary}
    write_json_atomic(path, value)


def is_proxy_source(summary: dict | None) -> bool:
    if not summary or summary.get("status") != "verified":
        return False
    identifier = str(summary.get("contract_identifier") or "").lower()
    names = {sig.split("(", 1)[0] for sig in (summary.get("abi") or {}).get("function_signatures") or []}
    return "proxy" in identifier or bool({"implementation", "upgradeTo", "upgradeToAndCall", "changeAdmin"} & names)


def load_proxy_cache(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    value = read_json(path)
    return value if isinstance(value, dict) else {}


def probe_proxy_addresses(
    ranked: list[dict], code_cache: dict[str, dict], proxy_cache: dict[str, dict], *,
    fetch: bool, rpc_url: str | None, family_limit: int, address_limit: int,
) -> dict[str, dict]:
    targets: list[str] = []
    for item in ranked[:family_limit]:
        source = normalize_source_summary(item.get("source_resolution"))
        family_proxy = is_proxy_source(source)
        for row in (item.get("top_addresses") or [])[:address_limit]:
            address = normalize_address(row.get("address"))
            if address is None:
                continue
            code = (code_cache.get(address) or {}).get("code")
            if detect_eip1167(code) is not None or family_proxy:
                targets.append(address)
    targets = sorted(set(targets))
    client = RpcClient(rpc_url) if fetch and rpc_url else None
    if fetch and client is None:
        raise ValueError("--fetch-proxies requires --rpc-url or ETH_RPC_URL")

    def ensure_implementation_code(implementation: str | None, block_number: int) -> None:
        if implementation is None or implementation in code_cache or client is None:
            return
        code = client.call("eth_getCode", [implementation, hex(block_number)])
        code_cache[implementation] = {"block_number": block_number, "code": "0x" + normalize_runtime_code(code)}
    for index, address in enumerate(targets, start=1):
        code_row = code_cache.get(address) or {}
        block_number = int(code_row.get("block_number", 0))
        eip1167 = detect_eip1167(code_row.get("code"))
        cached = proxy_cache.get(address)
        if eip1167 is not None:
            proxy_cache[address] = {
                "block_number": block_number,
                "resolution": "eip1167-runtime",
                "implementation_address": eip1167,
                "beacon_address": None,
            }
            ensure_implementation_code(eip1167, block_number)
            continue
        if cached and int(cached.get("block_number", -1)) == block_number and "implementation_address" in cached:
            continue
        if client is None:
            continue
        implementation_word = client.call("eth_getStorageAt", [address, EIP1967_IMPLEMENTATION_SLOT, hex(block_number)])
        beacon_word = client.call("eth_getStorageAt", [address, EIP1967_BEACON_SLOT, hex(block_number)])
        proxy_cache[address] = {
            "block_number": block_number,
            "resolution": "eip1967-slot-probe",
            "implementation_slot": str(implementation_word).lower(),
            "implementation_address": _address_from_storage_word(implementation_word),
            "beacon_slot": str(beacon_word).lower(),
            "beacon_address": _address_from_storage_word(beacon_word),
        }
        ensure_implementation_code(proxy_cache[address]["implementation_address"], block_number)
        if index == 1 or index % 25 == 0 or index == len(targets):
            print(f"background proxy [{index}/{len(targets)}] {address} impl={proxy_cache[address].get('implementation_address') or '-'}")
    return proxy_cache


def signatures_and_names(summary: dict | None) -> tuple[set[str], set[str]]:
    signatures = set(((summary or {}).get("abi") or {}).get("function_signatures") or [])
    names = {signature.split("(", 1)[0] for signature in signatures}
    return signatures, names


def classify_archetype(summary: dict | None) -> str | None:
    if not summary or summary.get("status") != "verified":
        return None
    signatures, names = signatures_and_names(summary)
    identifier = str(summary.get("contract_identifier") or "").lower()
    if "operatorfilterregistry" in identifier:
        return "operator-filter-helper"
    if "seaport" in identifier:
        return "marketplace-router"
    if "erc1155" in identifier or "safeTransferFrom(address,address,uint256,uint256,bytes)" in signatures or "balanceOf(address,uint256)" in signatures:
        return "cw1155-like"
    if "weth" in identifier and {"deposit", "withdraw", "transfer"}.issubset(names):
        return "wrapped-native-token"
    if {"getReserves", "swap", "token0", "token1"}.issubset(names):
        return "astroport-pair"
    if {"claimRank", "claimMintReward"} & names:
        return "xen-like"
    if "erc721" in identifier or ({"ownerOf", "setApprovalForAll"}.issubset(names) and ("safeTransferFrom" in names or "transferFrom" in names)):
        return "cw721-mintable"
    if {"transfer", "transferFrom", "balanceOf", "approve"}.issubset(names):
        controlled = bool({"blacklist", "unBlacklist", "pause", "unpause", "configureMinter", "masterMinter", "addBlackList"} & names)
        return "controlled-cw20" if controlled else "cw20-base"
    return None


ERC20_ENTRYPOINTS = {
    "transfer(address,uint256)": "execute::transfer",
    "transferFrom(address,address,uint256)": "execute::transfer_from",
    "balanceOf(address)": "query::balance",
    "approve(address,uint256)": "execute::approve",
    "allowance(address,address)": "query::allowance",
    "totalSupply()": "query::total_supply",
    "decimals()": "query::decimals",
    "increaseAllowance(address,uint256)": "execute::increase_allowance",
    "decreaseAllowance(address,uint256)": "execute::decrease_allowance",
    "burn(uint256)": "execute::burn",
}
ERC721_ENTRYPOINTS = {
    "safeTransferFrom(address,address,uint256)": "execute::transfer_nft",
    "safeTransferFrom(address,address,uint256,bytes)": "execute::send_or_safe_transfer_nft",
    "transferFrom(address,address,uint256)": "execute::transfer_nft",
    "setApprovalForAll(address,bool)": "execute::approve_all",
    "approve(address,uint256)": "execute::approve_nft",
    "ownerOf(uint256)": "query::owner_of",
    "balanceOf(address)": "query::tokens_by_owner_count",
    "getApproved(uint256)": "query::approval",
    "tokenURI(uint256)": "query::token_uri",
}
ERC1155_ENTRYPOINTS = {
    "safeTransferFrom(address,address,uint256,uint256,bytes)": "execute::send_from",
    "safeBatchTransferFrom(address,address,uint256[],uint256[],bytes)": "execute::batch_send_from",
    "balanceOf(address,uint256)": "query::balance",
    "balanceOfBatch(address[],uint256[])": "query::batch_balance",
    "setApprovalForAll(address,bool)": "execute::approve_all",
    "isApprovedForAll(address,address)": "query::approved_for_all",
}


def entrypoint_for_signature(archetype: str, signature: str) -> str | None:
    if archetype in {"cw20-base", "controlled-cw20", "fee-token-cw20"}:
        return ERC20_ENTRYPOINTS.get(signature)
    if archetype == "wrapped-native-token":
        extra = {"deposit()": "execute::deposit", "withdraw(uint256)": "execute::withdraw"}
        return extra.get(signature) or ERC20_ENTRYPOINTS.get(signature)
    if archetype == "cw721-mintable":
        return ERC721_ENTRYPOINTS.get(signature)
    if archetype == "cw1155-like":
        return ERC1155_ENTRYPOINTS.get(signature)
    if archetype == "astroport-pair":
        pair = {"getReserves()": "query::reserves", "swap(uint256,uint256,address,bytes)": "execute::swap", "token0()": "query::asset0", "token1()": "query::asset1", "sync()": "execute::sync", "mint(address)": "execute::provide_liquidity_mint"}
        return pair.get(signature)
    if archetype == "xen-like":
        xen = {"claimRank(uint256)": "execute::claim_rank", "claimMintReward()": "execute::claim_mint_reward", "claimMintRewardAndShare(address,uint256)": "execute::claim_mint_reward_and_share", "stake(uint256,uint256)": "execute::stake", "withdraw()": "execute::withdraw"}
        return xen.get(signature) or ERC20_ENTRYPOINTS.get(signature)
    if archetype == "operator-filter-helper":
        return "helper::" + signature.split("(", 1)[0]
    if archetype == "marketplace-router":
        return "marketplace::" + signature.split("(", 1)[0]
    return None


def selector_rules_for_source(
    *, family: str, observed_selectors: set[str], source: dict | None, source_address: str | None,
    address_scope: set[str] | None, resolution: str,
) -> list[dict]:
    archetype = classify_archetype(source)
    if archetype is None:
        return []
    selector_map = ((source or {}).get("abi") or {}).get("selector_to_signatures") or {}
    rules = []
    for selector in sorted(observed_selectors):
        candidates = selector_map.get(selector, [])
        accepted = [(signature, entrypoint_for_signature(archetype, signature)) for signature in candidates]
        accepted = [(signature, entrypoint) for signature, entrypoint in accepted if entrypoint]
        if len(accepted) != 1:
            continue
        signature, entrypoint = accepted[0]
        rules.append({
            "runtime_family": family,
            "selector": selector,
            "address_scope": sorted(address_scope) if address_scope else None,
            "native_code_family": archetype,
            "native_entrypoint": entrypoint,
            "ethereum_function_signature": signature,
            "source_address": source_address,
            "source_identifier": (source or {}).get("contract_identifier"),
            "evidence": "verified-abi-selector",
            "resolution": resolution,
        })
    return rules


def audited_custodial_batch_system_rules(
    plan: list[dict], code_cache: dict[str, dict], *, address: str, family: str,
    native_batch_selector: str = S3_CUSTODIAL_NATIVE_BATCH_SELECTOR,
    token_batch_selector: str = S3_CUSTODIAL_TOKEN_BATCH_SELECTOR,
) -> list[dict]:
    """Return address-scoped system/composition rules only when all audited call shapes match.

    This is intentionally stricter than interface heuristics. The target has no verified ABI/source,
    so the rules describe only behavior visible in the call plan: empty-calldata value deposits,
    native-value batch dispatch, and ERC-20 ``transfer`` batch dispatch. No function name or hidden
    contract state is inferred. The rule is omitted entirely if the expected shape is not present.
    """
    address = normalize_address(address)
    family = str(family).lower()
    if address is None or family_for_address(address, code_cache) != family:
        return []

    groups: dict[str, list[tuple[dict, list[dict]]]] = defaultdict(list)
    for block in plan:
        for tx in block.get("transactions") or []:
            actions = tx.get("native_actions") or []
            for index, action in enumerate(actions):
                if action.get("translation_status") != "background-fallback":
                    continue
                if normalize_address(action.get("ethereum_code_address")) != address:
                    continue
                if int(action.get("depth") or 0) != 0:
                    continue
                selector = str(action.get("selector") or "0x").lower()
                # A depth-0 action is the transaction root. All later actions in this flattened call
                # tree are descendants of that root; there is only one depth-0 action per tx.
                groups[selector].append((action, actions[index + 1 :]))

    deposit_rows = groups.get("0x", [])
    if not deposit_rows:
        return []
    if not all(
        str(root.get("call_type") or "").upper() == "CALL"
        and int_value(root.get("ethereum_value")) > 0
        and not descendants
        for root, descendants in deposit_rows
    ):
        return []

    native_rows = groups.get(native_batch_selector.lower(), [])
    if not native_rows:
        return []
    native_children = [child for root, descendants in native_rows for child in descendants]
    native_value_children = 0
    for child in native_children:
        if (
            str(child.get("call_type") or "").upper() == "CALL"
            and str(child.get("selector") or "0x").lower() == "0x"
            and int_value(child.get("ethereum_value")) > 0
        ):
            native_value_children += 1
    if not all(
        str(root.get("call_type") or "").upper() == "CALL" and int_value(root.get("ethereum_value")) == 0
        for root, _ in native_rows
    ):
        return []
    # Allow a small number of helper/authorization frames around the value sends, but require the
    # observed call composition to be overwhelmingly native-value dispatch.
    if not native_children or native_value_children / len(native_children) < 0.95:
        return []

    token_rows = groups.get(token_batch_selector.lower(), [])
    if not token_rows:
        return []
    token_children = [child for root, descendants in token_rows for child in descendants]
    if not all(
        str(root.get("call_type") or "").upper() == "CALL" and int_value(root.get("ethereum_value")) == 0
        for root, _ in token_rows
    ):
        return []
    # Every observed child of the token-batch entrypoint must be ERC-20 transfer-shaped. This covers
    # both direct token calls and inlined proxy delegatecalls while refusing unrelated composition.
    if not token_children or any(
        str(child.get("selector") or "0x").lower() != "0xa9059cbb" for child in token_children
    ):
        return []

    common = {
        "runtime_family": family,
        "address_scope": [address],
        "native_code_family": "system",
        "source_address": address,
        "source_identifier": None,
        "evidence": "audited-call-composition-no-source-abi",
        "resolution": "address-scoped-system-composition",
    }
    return [
        {
            **common,
            "selector": "0x",
            "native_entrypoint": "system::custodial_value_deposit",
            "ethereum_function_signature": None,
            "semantic_scope": "value-transfer-only",
        },
        {
            **common,
            "selector": native_batch_selector.lower(),
            "native_entrypoint": "system::batch_native_dispatch",
            "ethereum_function_signature": None,
            "semantic_scope": "composition-root-only",
        },
        {
            **common,
            "selector": token_batch_selector.lower(),
            "native_entrypoint": "system::batch_token_dispatch",
            "ethereum_function_signature": None,
            "semantic_scope": "composition-root-only",
        },
    ]


def collect_delegatecall_diagnostics(plan: list[dict]) -> dict[str, Counter[str]]:
    output: dict[str, Counter[str]] = defaultdict(Counter)
    for block in plan:
        for tx in block.get("transactions") or []:
            actions = tx.get("native_actions") or []
            by_id = {}
            for candidate in actions:
                action_id = candidate.get("action_id")
                if action_id is None:
                    continue
                try:
                    by_id[int(action_id)] = candidate
                except (TypeError, ValueError):
                    continue
            for action in actions:
                if str(action.get("call_type") or "").upper() != "DELEGATECALL":
                    continue
                parent = by_id.get(int(action.get("parent_action_id", -1)))
                owner = normalize_address((parent or {}).get("ethereum_code_address") or action.get("storage_context_address"))
                target = normalize_address(action.get("ethereum_code_address"))
                if owner and target:
                    output[owner][target] += 1
    return output


def observed_family_selectors(plan: list[dict], code_cache: dict[str, dict]) -> dict[str, set[str]]:
    result: dict[str, set[str]] = defaultdict(set)
    for block in plan:
        for tx in block.get("transactions") or []:
            for action in tx.get("native_actions") or []:
                if action.get("translation_status") != "background-fallback":
                    continue
                address = normalize_address(action.get("ethereum_code_address"))
                family = family_for_address(address, code_cache)
                if family:
                    result[family].add(str(action.get("selector") or "0x").lower())
    return result


def build_rules(
    plan: list[dict], gap: dict, code_cache: dict[str, dict], proxy_cache: dict[str, dict], summaries: dict[str, dict], *,
    family_limit: int, fetch_source_enabled: bool, characterization_dir: Path, chain_id: int,
    sourcify_base: str, etherscan_base: str, etherscan_key: str | None,
) -> tuple[list[dict], list[dict]]:
    observed = observed_family_selectors(plan, code_cache)
    http = HttpJsonClient()
    delegate_diagnostics = collect_delegatecall_diagnostics(plan)
    rules: list[dict] = []
    proxy_records: list[dict] = []
    for item in (gap.get("ranked") or [])[:family_limit]:
        if item.get("family_kind") != "runtime-code":
            continue
        family = str(item["family"])
        representative = normalize_address(item.get("representative_address"))
        source = normalize_source_summary(item.get("source_resolution")) or summaries.get(representative or "")
        if source is None and representative is not None and fetch_source_enabled:
            source = fetch_source(
                representative, chain_id=chain_id, sourcify_base=sourcify_base,
                etherscan_base=etherscan_base, etherscan_key=etherscan_key, client=http,
            )
            summaries[representative] = source
            persist_final_source_summary(characterization_dir, representative, source)
        proxy_like = is_proxy_source(source)
        family_addresses = [normalize_address(row.get("address")) for row in item.get("top_addresses") or []]
        family_addresses = [address for address in family_addresses if address]
        structurally_resolved: dict[tuple[str, str], set[str]] = defaultdict(set)
        unresolved_addresses: set[str] = set()
        for address in family_addresses:
            code = (code_cache.get(address) or {}).get("code")
            implementation = detect_eip1167(code)
            resolution = "eip1167-runtime" if implementation else None
            if implementation is None:
                cached = proxy_cache.get(address) or {}
                implementation = normalize_address(cached.get("implementation_address"))
                if implementation:
                    resolution = str(cached.get("resolution") or "eip1967-slot")
            if implementation:
                implementation_family = family_for_address(implementation, code_cache)
                if implementation_family is None:
                    unresolved_addresses.add(address)
                    proxy_records.append({"storage_address": address, "runtime_family": family, "implementation_address": implementation, "implementation_family": None, "status": "implementation-code-missing", "observed_delegatecall_targets": [{"address": target, "invocations": count} for target, count in delegate_diagnostics.get(address, Counter()).most_common(10)]})
                    continue
                impl_source = summaries.get(implementation)
                if impl_source is None and fetch_source_enabled:
                    impl_source = fetch_source(implementation, chain_id=chain_id, sourcify_base=sourcify_base, etherscan_base=etherscan_base, etherscan_key=etherscan_key, client=http)
                    summaries[implementation] = impl_source
                    persist_final_source_summary(characterization_dir, implementation, impl_source)
                structurally_resolved[(implementation_family, resolution or "structural-proxy")].add(address)
                proxy_records.append({"storage_address": address, "runtime_family": family, "implementation_address": implementation, "implementation_family": implementation_family, "status": resolution, "source_status": (impl_source or {}).get("status"), "source_identifier": (impl_source or {}).get("contract_identifier"), "observed_delegatecall_targets": [{"address": target, "invocations": count} for target, count in delegate_diagnostics.get(address, Counter()).most_common(10)]})
            elif proxy_like:
                unresolved_addresses.add(address)
                proxy_records.append({"storage_address": address, "runtime_family": family, "implementation_address": None, "implementation_family": None, "status": "proxy-source-unresolved", "observed_delegatecall_targets": [{"address": target, "invocations": count} for target, count in delegate_diagnostics.get(address, Counter()).most_common(10)]})
        if structurally_resolved:
            for (implementation_family, resolution), addresses in structurally_resolved.items():
                sample = next(iter(addresses))
                implementation = normalize_address((proxy_cache.get(sample) or {}).get("implementation_address")) or detect_eip1167((code_cache.get(sample) or {}).get("code"))
                impl_source = summaries.get(implementation or "")
                rules.extend(selector_rules_for_source(
                    family=family, observed_selectors=observed.get(family, set()), source=impl_source,
                    source_address=implementation, address_scope=addresses, resolution=resolution,
                ))
            # Do not apply representative proxy ABI to unresolved instances.
            if unresolved_addresses:
                continue
        if not proxy_like:
            rules.extend(selector_rules_for_source(
                family=family, observed_selectors=observed.get(family, set()), source=source,
                source_address=representative, address_scope=None, resolution="direct-runtime-source",
            ))
    # The highest-volume unresolved S3 family has no verified ABI. Admit its three address-scoped
    # system/composition rules only if the complete observed call shapes pass the strict audit above.
    rules.extend(audited_custodial_batch_system_rules(
        plan, code_cache, address=S3_CUSTODIAL_BATCH_ADDRESS, family=S3_CUSTODIAL_BATCH_FAMILY,
    ))

    # Deterministic dedupe.
    deduped: dict[tuple, dict] = {}
    for rule in rules:
        key = (rule["runtime_family"], rule["selector"], tuple(rule.get("address_scope") or []), rule["native_code_family"], rule["native_entrypoint"])
        deduped[key] = rule
    return sorted(deduped.values(), key=lambda row: (row["runtime_family"], row["selector"], row.get("address_scope") or [])), proxy_records


def rule_index(rules: list[dict]) -> dict[tuple[str, str], list[dict]]:
    index: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for rule in rules:
        index[(rule["runtime_family"], rule["selector"])].append(rule)
    return index


def matching_rule(action: dict, code_cache: dict[str, dict], index: dict[tuple[str, str], list[dict]]) -> dict | None:
    address = normalize_address(action.get("ethereum_code_address"))
    family = family_for_address(address, code_cache)
    selector = str(action.get("selector") or "0x").lower()
    if family is None:
        return None
    for rule in index.get((family, selector), []):
        scope = rule.get("address_scope")
        if not scope or address in scope:
            return rule
    return None


def simulate(plan: list[dict], code_cache: dict[str, dict], rules: list[dict]) -> dict:
    index = rule_index(rules)
    total_frames = semantic_frames = recovered_frames = 0
    total_transactions = semantic_transactions = fully_semantic = mixed = background = 0
    recovered_background_transactions = 0
    rule_hits: Counter[tuple[str, str, str]] = Counter()
    new_archetypes: Counter[str] = Counter()
    existing = {"cw20-base", "controlled-cw20", "fee-token-cw20", "astroport-pair", "wrapped-native-token", "cw721-mintable", "xen-like", "system"}
    for block in plan:
        for tx in block.get("transactions") or []:
            total_transactions += 1
            actions = tx.get("native_actions") or []
            remaining_fallback = 0
            has_semantic = False
            recovered_here = False
            for action in actions:
                total_frames += 1
                if action.get("translation_status") != "background-fallback":
                    semantic_frames += 1
                    has_semantic = True
                    continue
                rule = matching_rule(action, code_cache, index)
                if rule is None:
                    remaining_fallback += 1
                    continue
                semantic_frames += 1
                recovered_frames += 1
                has_semantic = True
                recovered_here = True
                rule_hits[(rule["runtime_family"], rule["selector"], rule["native_code_family"])] += 1
                if rule["native_code_family"] not in existing:
                    new_archetypes[rule["native_code_family"]] += 1
            if has_semantic:
                semantic_transactions += 1
                if remaining_fallback == 0:
                    fully_semantic += 1
                else:
                    mixed += 1
            else:
                background += 1
            if tx.get("translation_class") == "background-only" and recovered_here:
                recovered_background_transactions += 1
    return {
        "transactions": total_transactions,
        "semantic_transactions": semantic_transactions,
        "semantic_transaction_coverage": semantic_transactions / total_transactions if total_transactions else 1.0,
        "fully_semantic_transactions": fully_semantic,
        "mixed_semantic_fallback_transactions": mixed,
        "background_only_transactions": background,
        "recovered_background_transactions": recovered_background_transactions,
        "total_frames": total_frames,
        "semantic_frames": semantic_frames,
        "semantic_call_frame_coverage": semantic_frames / total_frames if total_frames else 1.0,
        "recovered_fallback_frames": recovered_frames,
        "additional_candidate_archetype_frame_hits": dict(sorted(new_archetypes.items())),
        "rule_hits": [
            {"runtime_family": family, "selector": selector, "native_code_family": native, "frames": count}
            for (family, selector, native), count in sorted(rule_hits.items(), key=lambda row: (-row[1], row[0]))
        ],
    }


def gate_rows(coverage: dict, simulation: dict, gate_config: dict) -> list[dict]:
    values = {
        "aggregate_source_conflict_coverage": (coverage.get("source_conflict_coverage") or {}).get("coverage"),
        "median_conflict_bearing_block_coverage": (coverage.get("block_balanced_conflict_coverage") or {}).get("median_coverage"),
        "semantic_transaction_coverage": simulation.get("semantic_transaction_coverage"),
        "semantic_call_frame_coverage": simulation.get("semantic_call_frame_coverage"),
    }
    rows = []
    for name, value in values.items():
        rule = (gate_config.get("metrics") or {}).get(name) or {}
        minimum = rule.get("minimum")
        passed = value is not None and minimum is not None and float(value) + 1e-12 >= float(minimum)
        rows.append({"metric": name, "measured": value, "minimum": minimum, "enforced": bool(rule.get("enforced")), "passed": passed, "status": "pass" if passed else "fail", "measurement_source": "simulation" if name.startswith("semantic_") else "translation-coverage"})
    return rows


def rank1_diagnostic(plan: list[dict], gap: dict, code_cache: dict[str, dict]) -> dict:
    ranked = gap.get("ranked") or []
    if not ranked:
        return {"status": "no-gap-families"}
    top = ranked[0]
    family = top.get("family")
    selectors = Counter()
    call_types = Counter()
    parent_addresses = Counter()
    addresses = Counter()
    tx_ids = set()
    root = internal = positive_value = 0
    positive_wei = 0
    delegate_targets = Counter()
    for block in plan:
        for tx in block.get("transactions") or []:
            actions = tx.get("native_actions") or []
            by_id = {int(action.get("action_id", -1)): action for action in actions}
            for action in actions:
                if action.get("translation_status") != "background-fallback":
                    continue
                address = normalize_address(action.get("ethereum_code_address"))
                if family_for_address(address, code_cache) != family:
                    continue
                tx_ids.add((int(block["block_number"]), int(tx["tx_index"])))
                addresses[address or "-"] += 1
                selectors[str(action.get("selector") or "0x").lower()] += 1
                call_types[str(action.get("call_type") or "UNKNOWN").upper()] += 1
                depth = int(action.get("depth", 0))
                root += int(depth == 0)
                internal += int(depth > 0)
                value = int(str(action.get("ethereum_value") or "0"), 0)
                if value > 0:
                    positive_value += 1
                    positive_wei += value
                parent_id = action.get("parent_action_id")
                parent = None
                if parent_id is not None:
                    try:
                        parent = by_id.get(int(parent_id))
                    except (TypeError, ValueError):
                        parent = None
                parent_address = normalize_address((parent or {}).get("ethereum_code_address"))
                if parent_address:
                    parent_addresses[parent_address] += 1
                if str(action.get("call_type") or "").upper() == "DELEGATECALL" and address:
                    delegate_targets[address] += 1
    return {
        "status": "diagnosed",
        "rank": 1,
        "runtime_family": family,
        "family_kind": top.get("family_kind"),
        "source_resolution": top.get("source_resolution"),
        "frames": sum(selectors.values()),
        "transactions": len(tx_ids),
        "root_frames": root,
        "internal_frames": internal,
        "positive_value_frames": positive_value,
        "positive_value_wei": positive_wei,
        "top_addresses": [{"address": key, "frames": count} for key, count in addresses.most_common(10)],
        "top_selectors": [{"selector": key, "frames": count} for key, count in selectors.most_common(12)],
        "call_types": [{"type": key, "frames": count} for key, count in call_types.most_common()],
        "top_parent_addresses": [{"address": key, "frames": count} for key, count in parent_addresses.most_common(10)],
        "delegatecall_targets": [{"address": key, "frames": count} for key, count in delegate_targets.most_common(10)],
        "interpretation": "diagnostic only; no semantic fold is inferred without verified source/ABI or structural proxy evidence",
    }


def render_simulation(report: dict) -> str:
    sim = report["simulation"]
    lines = [
        "Vegeta S3 selector-granular final mapping simulation",
        "",
        f"selector rules: {report['selector_rule_count']}",
        f"base native families: {report['base_native_code_families']}",
        f"additional candidate archetypes used: {', '.join(report['additional_candidate_archetypes_used']) or '-'}",
        f"semantic transactions: {sim['semantic_transactions']} / {sim['transactions']} ({sim['semantic_transaction_coverage']*100:.2f}%)",
        f"  fully semantic: {sim['fully_semantic_transactions']}",
        f"  mixed semantic+fallback: {sim['mixed_semantic_fallback_transactions']}",
        f"  background only: {sim['background_only_transactions']}",
        f"semantic call frames: {sim['semantic_frames']} / {sim['total_frames']} ({sim['semantic_call_frame_coverage']*100:.2f}%)",
        f"recovered fallback frames: {sim['recovered_fallback_frames']}",
        f"recovered background-only transactions: {sim['recovered_background_transactions']}",
        "",
        "Frozen pre-execution gates:",
    ]
    for gate in report["gates"]:
        lines.append(f"  {gate['metric']}: measured={gate['measured']*100:.2f}% minimum={gate['minimum']*100:.2f}% source={gate['measurement_source']} status={gate['status']}")
    lines.extend([
        "",
        f"all enforced gates pass in simulation: {'yes' if report['all_enforced_gates_pass'] else 'no'}",
        "",
        "Important: selector-granular reuse is a pre-execution semantic-coverage simulation, not native conflict-topology fidelity.",
        "Candidate archetypes still require real CosmWasm implementations and genuine source-derived symbolic analyses.",
    ])
    return "\n".join(lines) + "\n"


def render_rank1(report: dict) -> str:
    if report.get("status") != "diagnosed":
        return "Vegeta S3 rank-1 background-family diagnostic\n\nno family available\n"
    lines = [
        "Vegeta S3 rank-1 background-family diagnostic", "",
        f"runtime family: {report['runtime_family']}",
        f"frames: {report['frames']}",
        f"transactions: {report['transactions']}",
        f"root/internal frames: {report['root_frames']} / {report['internal_frames']}",
        f"positive-value frames: {report['positive_value_frames']} value_wei={report['positive_value_wei']}",
        f"source status: {(report.get('source_resolution') or {}).get('status', '-')}",
        "top selectors:",
    ]
    for row in report.get("top_selectors") or []:
        lines.append(f"  {row['selector']} frames={row['frames']}")
    lines.append("top parent addresses:")
    for row in report.get("top_parent_addresses") or []:
        lines.append(f"  {row['address']} frames={row['frames']}")
    lines.extend(["", report["interpretation"]])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-dir", type=Path, default=DEFAULT_PLAN_DIR)
    parser.add_argument("--characterization-dir", type=Path, default=DEFAULT_CHARACTERIZATION)
    parser.add_argument("--gate-config", type=Path, default=DEFAULT_GATE)
    parser.add_argument("--base-family-map", type=Path, default=DEFAULT_BASE_MAP)
    parser.add_argument("--family-limit", type=int, default=50)
    parser.add_argument("--proxy-family-limit", type=int, default=25)
    parser.add_argument("--proxy-address-limit", type=int, default=25)
    parser.add_argument("--fetch-proxies", action="store_true")
    parser.add_argument("--rpc-url", default=os.environ.get("ETH_RPC_URL"))
    parser.add_argument("--fetch-source", action="store_true")
    parser.add_argument("--chain-id", type=int, default=1)
    parser.add_argument("--sourcify-base", default="https://sourcify.dev/server")
    parser.add_argument("--etherscan-base", default="https://api.etherscan.io/v2/api")
    parser.add_argument("--etherscan-api-key", default=os.environ.get("ETHERSCAN_API_KEY"))
    args = parser.parse_args()

    plan = load_blocks(args.plan_dir / "native-plan.jsonl")
    gap = read_json(args.plan_dir / "background-gap-dossier.json")
    coverage = read_json(args.plan_dir / "translation-coverage.json")
    code_cache = load_code_cache(args.characterization_dir / "code-cache.json")
    summaries = load_summary_caches(args.characterization_dir)
    for item in gap.get("ranked") or []:
        address = normalize_address(item.get("representative_address"))
        source = normalize_source_summary(item.get("source_resolution"))
        if address and source:
            summaries[address] = source

    proxy_cache_path = args.characterization_dir / "background-proxy-resolution-cache.json"
    proxy_cache = load_proxy_cache(proxy_cache_path)
    proxy_cache = probe_proxy_addresses(
        gap.get("ranked") or [], code_cache, proxy_cache,
        fetch=args.fetch_proxies, rpc_url=args.rpc_url,
        family_limit=max(0, args.proxy_family_limit), address_limit=max(1, args.proxy_address_limit),
    )
    if args.fetch_proxies:
        write_json_atomic(proxy_cache_path, proxy_cache)
        # Newly discovered structural implementations may not have been part of the original relevant
        # code crawl. Persist their historical runtime code so subsequent offline runs remain complete.
        write_json_atomic(args.characterization_dir / "code-cache.json", code_cache)

    rules, proxy_records = build_rules(
        plan, gap, code_cache, proxy_cache, summaries,
        family_limit=max(1, args.family_limit), fetch_source_enabled=args.fetch_source,
        characterization_dir=args.characterization_dir, chain_id=args.chain_id,
        sourcify_base=args.sourcify_base, etherscan_base=args.etherscan_base,
        etherscan_key=args.etherscan_api_key,
    )
    simulation = simulate(plan, code_cache, rules)
    gates = gate_rows(coverage, simulation, read_json(args.gate_config))
    all_pass = all((not gate["enforced"]) or gate["passed"] for gate in gates)
    base_map = read_json(args.base_family_map)
    base_families = sorted((base_map.get("native_code_families") or {}).keys())
    candidate_archetypes = sorted(
        set(rule["native_code_family"] for rule in rules) - set(base_families) - {"system"}
    )
    rank1 = rank1_diagnostic(plan, gap, code_cache)

    final_map = {
        "schema_version": "2-selector-granular-preexecution-simulation",
        "dataset": "vegeta-s3",
        "status": "simulation-only-not-execution-ready",
        "base_family_map": str(args.base_family_map),
        "base_native_code_families": base_families,
        "additional_candidate_archetypes": {
            "cw1155-like": {"status": "candidate-needs-native-contract-and-symbolic-analysis"},
            "marketplace-router": {"status": "candidate-needs-native-contract-and-symbolic-analysis"},
            "operator-filter-helper": {"status": "candidate-needs-native-contract-and-symbolic-analysis"},
        },
        "address_scoped_system_semantics": {
            "status": "call-composition-audited-not-source-derived",
            "address": S3_CUSTODIAL_BATCH_ADDRESS,
            "runtime_family": S3_CUSTODIAL_BATCH_FAMILY,
            "note": "models only audited value-deposit and batch-dispatch call shapes; no hidden ABI/state semantics are inferred",
        },
        "selector_rules": rules,
        "proxy_resolution": {
            "cache": str(proxy_cache_path),
            "structural_only": True,
            "records": proxy_records,
            "note": "generic DELEGATECALL is diagnostic only; EIP-1167/EIP-1967 evidence is required to rewrite proxy source semantics",
        },
        "rules": [
            "storage namespaces are never merged across Ethereum addresses",
            "reuse is selector-granular; unsupported custom selectors remain fallback",
            "proxy-family rules may be address-scoped when implementations differ",
            "candidate archetypes are not executable until real CosmWasm code and genuine LLM symbolic analysis exist",
            "address-scoped system/composition rules with unverified source model only audited public call shapes and do not claim full contract equivalence",
        ],
    }
    write_json_atomic(args.plan_dir / "final-native-family-map.v2.json", final_map)
    write_json_atomic(args.plan_dir / "selector-semantic-map.json", {"schema_version": 1, "dataset": "vegeta-s3", "rules": rules})
    write_json_atomic(args.plan_dir / "background-proxy-resolution.json", {"schema_version": 1, "records": proxy_records})
    write_json_atomic(args.plan_dir / "background-rank1-diagnostic.json", rank1)
    (args.plan_dir / "background-rank1-diagnostic.txt").write_text(render_rank1(rank1), encoding="utf-8")
    report = {
        "schema_version": 1,
        "dataset": "vegeta-s3",
        "selector_rule_count": len(rules),
        "base_native_code_families": len(base_families),
        "additional_candidate_archetypes_used": candidate_archetypes,
        "simulation": simulation,
        "gates": gates,
        "all_enforced_gates_pass": all_pass,
        "rank1_background_family": rank1.get("runtime_family"),
        "prediction_leakage_note": "simulation uses call-plan runtime-family/selector/source metadata only; no historical concrete reads/writes are consumed",
    }
    write_json_atomic(args.plan_dir / "final-mapping-simulation.json", report)
    text = render_simulation(report)
    (args.plan_dir / "final-mapping-simulation.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
