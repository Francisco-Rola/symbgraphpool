#!/usr/bin/env python3
"""Characterize Vegeta's Ethereum corpus by contract, method, storage owner, and code family.

This is intentionally a corpus-analysis tool rather than part of the execution harness. It answers
whether the S3 trace can be represented by a tractable set of real contract/code families before we
attempt a native CosmWasm port with source-derived symbolic profiles.

Offline characterization reports:

* direct destination-address and function-selector frequencies;
* (destination, selector) method frequencies;
* storage-owner access volume derived from canonical ``evm/<address>/<slot>`` keys;
* same-block read/write conflict pairs, attributed to the storage owner whose key caused them;
* top-N coverage curves for destinations, methods, and storage owners.

With ``--fetch-calls``, the tool traces each block with Geth's built-in ``callTracer`` and records
root and internal contract invocations, selectors, call types, and delegatecall edges. Traces are
written one block at a time under ``call-cache/`` so an interrupted public-RPC run is resumable.

With ``--fetch-code``, the tool calls ``eth_getCode`` and groups relevant addresses by SHA-256 of
their runtime bytecode. ``--code-scope relevant`` covers direct destinations, storage owners, and
callTracer callees; the default snapshot is each address's first-seen corpus block. When both calls
and relevant code are available, the tool emits a conflict-weighted native-port candidate ranking.
The ranking attributes conflicts to the bytecode family of the storage-owning address and reports
callTracer invocation frequency separately; it does not pretend that prestateTracer storage can be
assigned to an individual call frame. With ``--native-family-mapping-candidates``, conflict-owning addresses are additionally probed for
standard EIP-1967 implementation/beacon slots. Exact EIP-1167 bytecode and non-zero EIP-1967
implementation slots can rewrite the recommended source-analysis/profile family while preserving the
original storage namespace. Generic DELEGATECALL edges remain unresolved candidates because the
storage trace cannot attribute each access to a call frame.
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
import urllib.request
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterable, Iterator

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from vegeta_corpus import canonical_accesses, load_blocks, storage_contract  # noqa: E402

DEFAULT_COVERAGE_POINTS = (1, 5, 10, 25, 50, 100)
CREATE_SENTINEL = "<create>"
EMPTY_CODE_FAMILY = "<empty-code>"
EIP1167_RE = re.compile(
    r"^363d3d373d3d3d363d73([0-9a-f]{40})5af43d82803e903d91602b57fd5bf3$"
)
EIP1967_IMPLEMENTATION_SLOT = "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc"
EIP1967_BEACON_SLOT = "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50"

KNOWN_INTERFACE_SELECTORS = {
    "fungible-token-like": {"0xa9059cbb", "0x70a08231", "0x23b872dd", "0x095ea7b3", "0xdd62ed3e"},
    "wrapped-native-token-like": {"0xd0e30db0", "0x2e1a7d4d"},
    "constant-product-amm-pair-like": {"0x0902f1ac", "0x022c0d9f"},
    "nft-like": {"0x42842e0e", "0xb88d4fde", "0xa22cb465"},
}


def normalize_address(value: str | None) -> str | None:
    if value is None:
        return None
    value = str(value).lower()
    if value == CREATE_SENTINEL:
        return None
    if value.startswith("0x"):
        value = value[2:]
    if len(value) != 40:
        return None
    try:
        int(value, 16)
    except ValueError:
        return None
    return "0x" + value


def address_from_storage_key(key: str) -> str | None:
    contract = storage_contract(key)
    return "0x" + contract if contract is not None else None


def sorted_counter(counter: Counter, limit: int | None = None) -> list[tuple[object, int]]:
    values = sorted(counter.items(), key=lambda item: (-item[1], str(item[0])))
    return values if limit is None else values[:limit]


def coverage_curve(counter: Counter, total: int, points: Iterable[int] = DEFAULT_COVERAGE_POINTS) -> list[dict]:
    ranked = sorted_counter(counter)
    result = []
    cumulative = 0
    point_set = sorted({point for point in points if point > 0})
    point_index = 0
    for rank, (_, count) in enumerate(ranked, start=1):
        cumulative += count
        while point_index < len(point_set) and rank >= point_set[point_index]:
            top_n = point_set[point_index]
            result.append(
                {
                    "top_n": top_n,
                    "count": cumulative,
                    "coverage": (cumulative / total) if total else 0.0,
                }
            )
            point_index += 1
    while point_index < len(point_set):
        result.append(
            {
                "top_n": point_set[point_index],
                "count": cumulative,
                "coverage": (cumulative / total) if total else 0.0,
            }
        )
        point_index += 1
    return result


def _method_key(destination: str, selector: str) -> str:
    return f"{destination}:{selector.lower()}"


def _conflict_pairs_for_key(readers: set[int], writers: set[int]) -> set[tuple[int, int]]:
    """Return unordered tx pairs that conflict on one key (at least one side writes)."""

    touched = readers | writers
    ordered = sorted(touched)
    conflicts: set[tuple[int, int]] = set()
    for offset, left in enumerate(ordered):
        for right in ordered[offset + 1 :]:
            if left in writers or right in writers:
                conflicts.add((left, right))
    return conflicts


def characterize_blocks(blocks: list[dict]) -> dict:
    destination_transactions: Counter[str] = Counter()
    destination_failed: Counter[str] = Counter()
    destination_gas: Counter[str] = Counter()
    destination_selectors: dict[str, Counter[str]] = defaultdict(Counter)
    selectors: Counter[str] = Counter()
    methods: Counter[str] = Counter()
    first_seen_block: dict[str, int] = {}

    storage_transactions: Counter[str] = Counter()
    storage_access_records: Counter[str] = Counter()
    storage_reads: Counter[str] = Counter()
    storage_writes: Counter[str] = Counter()
    storage_unique_keys: dict[str, set[str]] = defaultdict(set)
    storage_conflict_pairs: Counter[str] = Counter()
    storage_conflict_keys: Counter[str] = Counter()
    storage_first_seen_block: dict[str, int] = {}

    total_transactions = 0
    failed_transactions = 0
    contract_creations = 0
    total_reads = 0
    total_writes = 0
    global_conflict_pairs = 0
    conflict_blocks = 0

    for block in blocks:
        block_number = int(block.get("block_number", -1))
        transactions = block.get("transactions", [])
        total_transactions += len(transactions)

        # Build a block-local inverted storage-key index. This intentionally avoids an O(n^2)
        # transaction-pair scan and mirrors the structure we want to reason about for the real
        # candidate graph later.
        key_readers: dict[str, set[int]] = defaultdict(set)
        key_writers: dict[str, set[int]] = defaultdict(set)
        per_tx_storage_contracts: list[set[str]] = []

        for index, tx in enumerate(transactions):
            destination = normalize_address(tx.get("to"))
            selector = str(tx.get("selector") or "0x").lower()
            if destination is None:
                contract_creations += 1
            else:
                destination_transactions[destination] += 1
                destination_selectors[destination][selector] += 1
                selectors[selector] += 1
                methods[_method_key(destination, selector)] += 1
                first_seen_block.setdefault(destination, block_number)
                if tx.get("failed"):
                    destination_failed[destination] += 1
                destination_gas[destination] += int(tx.get("gas_used", 0) or 0)

            if tx.get("failed"):
                failed_transactions += 1

            reads = set(tx.get("reads", ()))
            writes = set(tx.get("writes", ()))
            total_reads += len(reads)
            total_writes += len(writes)

            touched_contracts: set[str] = set()
            for key in reads:
                key_readers[key].add(index)
                owner = address_from_storage_key(key)
                if owner is not None:
                    storage_reads[owner] += 1
                    storage_access_records[owner] += 1
                    storage_unique_keys[owner].add(key)
                    storage_first_seen_block.setdefault(owner, block_number)
                    touched_contracts.add(owner)
            for key in writes:
                key_writers[key].add(index)
                owner = address_from_storage_key(key)
                if owner is not None:
                    storage_writes[owner] += 1
                    storage_access_records[owner] += 1
                    storage_unique_keys[owner].add(key)
                    storage_first_seen_block.setdefault(owner, block_number)
                    touched_contracts.add(owner)
            per_tx_storage_contracts.append(touched_contracts)

        for touched_contracts in per_tx_storage_contracts:
            for owner in touched_contracts:
                storage_transactions[owner] += 1

        block_conflicts: set[tuple[int, int]] = set()
        block_contract_conflicts: dict[str, set[tuple[int, int]]] = defaultdict(set)
        block_contract_conflict_keys: dict[str, set[str]] = defaultdict(set)
        for key in set(key_readers) | set(key_writers):
            writers = key_writers.get(key, set())
            if not writers:
                continue
            conflicts = _conflict_pairs_for_key(key_readers.get(key, set()), writers)
            if not conflicts:
                continue
            block_conflicts.update(conflicts)
            owner = address_from_storage_key(key)
            if owner is not None:
                block_contract_conflicts[owner].update(conflicts)
                block_contract_conflict_keys[owner].add(key)

        if block_conflicts:
            conflict_blocks += 1
        global_conflict_pairs += len(block_conflicts)
        for owner, pairs in block_contract_conflicts.items():
            storage_conflict_pairs[owner] += len(pairs)
        for owner, keys in block_contract_conflict_keys.items():
            storage_conflict_keys[owner] += len(keys)

    top_destinations = []
    for address, tx_count in sorted_counter(destination_transactions):
        selector_items = sorted_counter(destination_selectors[address], 10)
        top_destinations.append(
            {
                "address": address,
                "transactions": tx_count,
                "tx_coverage": (tx_count / total_transactions) if total_transactions else 0.0,
                "failed_transactions": destination_failed[address],
                "gas_used_sum": destination_gas[address],
                "first_seen_block": first_seen_block[address],
                "unique_selectors": len(destination_selectors[address]),
                "top_selectors": [
                    {"selector": selector, "transactions": count}
                    for selector, count in selector_items
                ],
            }
        )

    top_methods = []
    for method, count in sorted_counter(methods):
        address, selector = method.rsplit(":", 1)
        top_methods.append(
            {
                "address": address,
                "selector": selector,
                "transactions": count,
                "tx_coverage": (count / total_transactions) if total_transactions else 0.0,
            }
        )

    top_storage_contracts = []
    for owner, tx_count in sorted_counter(storage_transactions):
        top_storage_contracts.append(
            {
                "address": owner,
                "transactions_touching_storage": tx_count,
                "tx_coverage": (tx_count / total_transactions) if total_transactions else 0.0,
                "access_records": storage_access_records[owner],
                "read_records": storage_reads[owner],
                "write_records": storage_writes[owner],
                "unique_storage_keys": len(storage_unique_keys[owner]),
                "conflict_pairs": storage_conflict_pairs[owner],
                "conflict_keys": storage_conflict_keys[owner],
            }
        )

    result = {
        "schema_version": 1,
        "corpus": {
            "blocks": len(blocks),
            "first_block": int(blocks[0]["block_number"]) if blocks else None,
            "last_block": int(blocks[-1]["block_number"]) if blocks else None,
            "transactions": total_transactions,
            "failed_transactions": failed_transactions,
            "contract_creations": contract_creations,
        },
        "direct_destinations": {
            "unique_addresses": len(destination_transactions),
            "transactions_with_destination": sum(destination_transactions.values()),
            "coverage": coverage_curve(destination_transactions, total_transactions),
            "ranked": top_destinations,
        },
        "selectors": {
            "unique_selectors": len(selectors),
            "coverage": coverage_curve(selectors, sum(selectors.values())),
            "ranked": [
                {"selector": selector, "transactions": count}
                for selector, count in sorted_counter(selectors)
            ],
        },
        "methods": {
            "unique_destination_selector_pairs": len(methods),
            "coverage": coverage_curve(methods, total_transactions),
            "ranked": top_methods,
        },
        "storage": {
            "total_read_records": total_reads,
            "total_write_records": total_writes,
            "unique_storage_owners": len(storage_transactions),
            "coverage": coverage_curve(storage_transactions, total_transactions),
            "ranked": top_storage_contracts,
        },
        "conflicts": {
            "definition": "same-block tx pair touches the same canonical storage key and at least one tx writes",
            "blocks_with_conflicts": conflict_blocks,
            "total_unique_tx_pairs": global_conflict_pairs,
            "storage_owner_pair_attributions": sum(storage_conflict_pairs.values()),
        },
        "address_first_seen_block": dict(sorted(first_seen_block.items())),
        "storage_owner_first_seen_block": dict(sorted(storage_first_seen_block.items())),
    }
    return result


class RpcClient:
    def __init__(self, url: str, timeout: int = 30, retries: int = 5, backoff: float = 1.0):
        self.url = url
        self.timeout = timeout
        self.retries = retries
        self.backoff = backoff
        self.request_id = 0

    def call(self, method: str, params: list):
        last_error: Exception | None = None
        for attempt in range(self.retries + 1):
            self.request_id += 1
            body = json.dumps(
                {"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}
            ).encode()
            request = urllib.request.Request(
                self.url,
                data=body,
                headers={
                    "Content-Type": "application/json",
                    "User-Agent": "symbgraphpool-vegeta-characterizer/1",
                },
            )
            try:
                with urllib.request.urlopen(request, timeout=self.timeout) as response:
                    payload = json.load(response)
                if payload.get("error") is not None:
                    raise RuntimeError(f"RPC {method} returned error: {payload['error']}")
                return payload.get("result")
            except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, RuntimeError) as error:
                last_error = error
                retryable = True
                if isinstance(error, urllib.error.HTTPError):
                    retryable = error.code in {408, 425, 429, 500, 502, 503, 504}
                if attempt >= self.retries or not retryable:
                    raise RuntimeError(f"RPC {method} failed: {error}") from error
                delay = self.backoff * (2**attempt)
                print(
                    f"RPC {method} attempt {attempt + 1} failed; retrying in {delay:.1f}s: {error}",
                    file=sys.stderr,
                    flush=True,
                )
                time.sleep(delay)
        assert last_error is not None
        raise last_error


def normalize_runtime_code(code: str | None) -> str:
    if code is None:
        return ""
    normalized = str(code).lower()
    if normalized.startswith("0x"):
        normalized = normalized[2:]
    if not normalized:
        return ""
    if len(normalized) % 2:
        normalized = "0" + normalized
    try:
        bytes.fromhex(normalized)
    except ValueError as error:
        raise RuntimeError(f"eth_getCode returned non-hex bytecode: {code!r}") from error
    return normalized


def runtime_code_sha256(code_hex: str) -> str:
    return hashlib.sha256(bytes.fromhex(code_hex)).hexdigest()


def detect_eip1167_implementation(code_hex: str) -> str | None:
    match = EIP1167_RE.fullmatch(code_hex)
    return "0x" + match.group(1) if match else None


def load_code_cache(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise RuntimeError(f"code cache {path} is not a JSON object")
    return value


def write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_suffix(path.suffix + ".tmp")
    temp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temp.replace(path)


def fetch_runtime_codes(
    targets: dict[str, int],
    client: RpcClient,
    cache_path: Path,
    fixed_block: int | None,
    delay_ms: int,
) -> dict[str, dict]:
    """Fetch historical runtime code for address->first-seen-block targets, resumably."""

    cache = load_code_cache(cache_path)
    addresses = sorted(targets)

    for index, address in enumerate(addresses, start=1):
        target_block = fixed_block if fixed_block is not None else int(targets[address])
        cached = cache.get(address)
        if cached and int(cached.get("block_number", -1)) == target_block and "code" in cached:
            continue
        code = client.call("eth_getCode", [address, hex(target_block)])
        normalized = normalize_runtime_code(code)
        cache[address] = {
            "block_number": target_block,
            "code": "0x" + normalized,
        }
        # Persist after every address so a public-RPC interruption can resume safely.
        write_json_atomic(cache_path, cache)
        if index == 1 or index % 25 == 0 or index == len(addresses):
            print(
                f"code [{index}/{len(addresses)}] {address} @ {target_block} "
                f"bytes={len(normalized) // 2}",
                flush=True,
            )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
    return cache



def _address_from_storage_word(value: str | None) -> str | None:
    raw = str(value or "0x").lower()
    if raw.startswith("0x"):
        raw = raw[2:]
    if not raw:
        return None
    if len(raw) > 64:
        return None
    try:
        int(raw, 16)
    except ValueError:
        return None
    raw = raw.rjust(64, "0")
    if int(raw, 16) == 0:
        return None
    return normalize_address(raw[-40:])


def load_proxy_resolution_cache(path: Path) -> dict[str, dict]:
    if not path.exists():
        return {}
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise RuntimeError(f"proxy resolution cache {path} is not a JSON object")
    return value


def fetch_eip1967_slots(
    targets: dict[str, int],
    client: RpcClient,
    cache_path: Path,
    fixed_block: int | None,
    delay_ms: int,
) -> dict[str, dict]:
    """Probe standard EIP-1967 implementation/beacon slots for conflict-owning addresses.

    A zero slot is retained as a successful negative observation. The cache is block-scoped and
    updated after every address so an interrupted public-RPC pass is resumable.
    """

    cache = load_proxy_resolution_cache(cache_path)
    addresses = sorted(targets)
    for index, address in enumerate(addresses, start=1):
        target_block = fixed_block if fixed_block is not None else int(targets[address])
        cached = cache.get(address)
        if (
            cached
            and int(cached.get("block_number", -1)) == target_block
            and "implementation_slot" in cached
            and "beacon_slot" in cached
        ):
            continue
        implementation_word = client.call(
            "eth_getStorageAt", [address, EIP1967_IMPLEMENTATION_SLOT, hex(target_block)]
        )
        beacon_word = client.call(
            "eth_getStorageAt", [address, EIP1967_BEACON_SLOT, hex(target_block)]
        )
        cache[address] = {
            "block_number": target_block,
            "implementation_slot": str(implementation_word).lower(),
            "implementation_address": _address_from_storage_word(implementation_word),
            "beacon_slot": str(beacon_word).lower(),
            "beacon_address": _address_from_storage_word(beacon_word),
        }
        write_json_atomic(cache_path, cache)
        if index == 1 or index % 25 == 0 or index == len(addresses):
            impl = cache[address]["implementation_address"] or "-"
            beacon = cache[address]["beacon_address"] or "-"
            print(
                f"proxy [{index}/{len(addresses)}] {address} @ {target_block} "
                f"implementation={impl} beacon={beacon}",
                flush=True,
            )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
    return cache


def proxy_probe_targets(characterization: dict) -> dict[str, int]:
    first_seen = characterization.get("storage_owner_first_seen_block", {})
    targets = {}
    for item in characterization.get("storage", {}).get("ranked", []):
        if int(item.get("conflict_pairs", 0)) <= 0:
            continue
        address = normalize_address(item.get("address"))
        if address is None or address not in first_seen:
            continue
        targets[address] = int(first_seen[address])
    return targets


def proxy_implementation_code_targets(
    proxy_cache: dict[str, dict],
    characterization: dict | None = None,
    code_cache: dict[str, dict] | None = None,
) -> dict[str, int]:
    targets: dict[str, int] = {}
    for entry in proxy_cache.values():
        implementation = normalize_address(entry.get("implementation_address"))
        if implementation is None:
            continue
        block_number = int(entry["block_number"])
        targets[implementation] = min(block_number, targets.get(implementation, block_number))

    # EIP-1167 embeds its implementation directly in the proxy runtime bytecode, so include that
    # structural target even if a provider omitted or pruned the corresponding callTracer frame.
    if characterization is not None and code_cache is not None:
        first_seen = characterization.get("storage_owner_first_seen_block", {})
        for item in characterization.get("storage", {}).get("ranked", []):
            if int(item.get("conflict_pairs", 0)) <= 0:
                continue
            owner = normalize_address(item.get("address"))
            if owner is None or owner not in code_cache or owner not in first_seen:
                continue
            owner_code = normalize_runtime_code(code_cache[owner].get("code"))
            implementation = detect_eip1167_implementation(owner_code) if owner_code else None
            if implementation is None:
                continue
            block_number = int(first_seen[owner])
            targets[implementation] = min(block_number, targets.get(implementation, block_number))
    return targets

def code_family_for_address(code_cache: dict[str, dict], address: str) -> str | None:
    entry = code_cache.get(address)
    if entry is None:
        return None
    code_hex = normalize_runtime_code(entry.get("code"))
    return runtime_code_sha256(code_hex) if code_hex else EMPTY_CODE_FAMILY


def code_family_index(code_cache: dict[str, dict], addresses: Iterable[str] | None = None) -> dict[str, str]:
    selected = set(addresses) if addresses is not None else set(code_cache)
    result: dict[str, str] = {}
    for address in selected:
        family = code_family_for_address(code_cache, address)
        if family is not None:
            result[address] = family
    return result


UNMAPPED_CODE_FAMILY = "<unmapped-code>"
CALL_TYPES_WITH_INPUT = {"CALL", "STATICCALL", "DELEGATECALL", "CALLCODE"}
SELFDESTRUCT_TYPES = {"SELFDESTRUCT", "SUICIDE"}


def selector_from_input(value: str | None) -> str:
    data = str(value or "0x").lower()
    if not data.startswith("0x"):
        data = "0x" + data
    return data[:10] if len(data) >= 10 else "0x"


def iter_call_frames(
    frame: dict, depth: int = 0, parent_address: str | None = None
) -> Iterator[tuple[dict, int, str | None]]:
    if not isinstance(frame, dict):
        return
    yield frame, depth, parent_address
    current_address = normalize_address(frame.get("to")) or parent_address
    calls = frame.get("calls") or []
    if not isinstance(calls, list):
        return
    for child in calls:
        if isinstance(child, dict):
            yield from iter_call_frames(child, depth + 1, current_address)


def count_call_frames(frame: dict) -> int:
    return sum(1 for _ in iter_call_frames(frame))


def normalize_block_call_trace_item(item: object, expected_hash: str, block_number: int, index: int) -> dict:
    if not isinstance(item, dict):
        raise RuntimeError(f"block {block_number} tx {index}: callTracer result is not an object")
    observed_hash = item.get("txHash") or item.get("transactionHash")
    if observed_hash is not None and str(observed_hash).lower() != expected_hash.lower():
        raise RuntimeError(
            f"block {block_number} tx {index}: callTracer hash {observed_hash} does not match {expected_hash}"
        )
    result = item.get("result") if "result" in item else item
    if not isinstance(result, dict):
        raise RuntimeError(f"block {block_number} tx {index}: callTracer frame is not an object")
    return {"tx_hash": expected_hash.lower(), "result": result}


def load_cached_call_block(path: Path, block: dict) -> dict | None:
    if not path.exists():
        return None
    try:
        cached = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    if not isinstance(cached, dict):
        return None
    if int(cached.get("block_number", -1)) != int(block.get("block_number", -2)):
        return None
    expected_block_hash = str(block.get("block_hash") or "").lower()
    cached_block_hash = str(cached.get("block_hash") or "").lower()
    if expected_block_hash and cached_block_hash != expected_block_hash:
        return None
    expected = block.get("transactions") or []
    observed = cached.get("transactions") or []
    if len(expected) != len(observed):
        return None
    for source_tx, traced_tx in zip(expected, observed):
        if str(source_tx.get("tx_hash") or "").lower() != str(traced_tx.get("tx_hash") or "").lower():
            return None
        if not isinstance(traced_tx.get("result"), dict):
            return None
    return cached


def fetch_call_traces(
    blocks: list[dict],
    client: RpcClient,
    cache_dir: Path,
    trace_timeout_seconds: int,
    reexec: int,
    delay_ms: int,
) -> dict[int, dict]:
    """Fetch Geth callTracer trees one block at a time with an atomic resumable cache."""

    cache_dir.mkdir(parents=True, exist_ok=True)
    result: dict[int, dict] = {}
    total = len(blocks)
    for position, block in enumerate(blocks, start=1):
        block_number = int(block["block_number"])
        cache_path = cache_dir / f"{block_number}.json"
        cached = load_cached_call_block(cache_path, block)
        if cached is not None:
            result[block_number] = cached
            if position == 1 or position % 10 == 0 or position == total:
                print(f"calls [{position}/{total}] reuse block {block_number}", flush=True)
            continue

        config = {
            "tracer": "callTracer",
            "tracerConfig": {"onlyTopCall": False, "withLog": False},
            "timeout": f"{trace_timeout_seconds}s",
            "reexec": reexec,
        }
        raw = client.call("debug_traceBlockByNumber", [hex(block_number), config])
        if not isinstance(raw, list):
            raise RuntimeError(f"block {block_number}: debug_traceBlockByNumber did not return a list")
        transactions = block.get("transactions") or []
        if len(raw) != len(transactions):
            raise RuntimeError(
                f"block {block_number}: {len(transactions)} corpus transactions but {len(raw)} call traces"
            )
        traced_transactions = []
        frames = 0
        for index, (tx, item) in enumerate(zip(transactions, raw)):
            tx_hash = str(tx.get("tx_hash") or "").lower()
            if not tx_hash:
                raise RuntimeError(f"block {block_number} tx {index}: corpus transaction has no tx_hash")
            normalized = normalize_block_call_trace_item(item, tx_hash, block_number, index)
            frames += count_call_frames(normalized["result"])
            traced_transactions.append(normalized)
        cached = {
            "schema_version": 1,
            "trace_semantics": "geth-callTracer-v1",
            "block_number": block_number,
            "block_hash": str(block.get("block_hash") or "").lower(),
            "transactions": traced_transactions,
        }
        write_json_atomic(cache_path, cached)
        result[block_number] = cached
        print(
            f"calls [{position}/{total}] block {block_number} tx={len(transactions)} frames={frames}",
            flush=True,
        )
        if delay_ms > 0:
            time.sleep(delay_ms / 1000.0)
    return result


def summarize_call_traces(blocks: list[dict], call_blocks: dict[int, dict]) -> dict:
    invocation_count: Counter[str] = Counter()
    internal_invocation_count: Counter[str] = Counter()
    root_invocation_count: Counter[str] = Counter()
    failed_frames: Counter[str] = Counter()
    call_types: dict[str, Counter[str]] = defaultdict(Counter)
    selectors: dict[str, Counter[str]] = defaultdict(Counter)
    tx_ids: dict[str, set[tuple[int, int]]] = defaultdict(set)
    first_seen: dict[str, int] = {}
    call_edges: Counter[tuple[str, str, str]] = Counter()
    delegatecall_edges: Counter[tuple[str, str]] = Counter()
    total_frames = 0
    internal_frames = 0

    for block in blocks:
        block_number = int(block["block_number"])
        cached = call_blocks.get(block_number)
        if cached is None:
            continue
        traces = cached.get("transactions") or []
        for tx_index, traced_tx in enumerate(traces):
            root = traced_tx.get("result") or {}
            for frame, depth, parent_address in iter_call_frames(root):
                total_frames += 1
                if depth > 0:
                    internal_frames += 1
                call_type = str(frame.get("type") or "UNKNOWN").upper()
                if call_type in SELFDESTRUCT_TYPES:
                    continue
                address = normalize_address(frame.get("to"))
                if address is None:
                    continue
                first_seen.setdefault(address, block_number)
                invocation_count[address] += 1
                tx_ids[address].add((block_number, tx_index))
                if depth == 0:
                    root_invocation_count[address] += 1
                else:
                    internal_invocation_count[address] += 1
                call_types[address][call_type] += 1
                selector = selector_from_input(frame.get("input")) if call_type in CALL_TYPES_WITH_INPUT else "0x"
                selectors[address][selector] += 1
                if frame.get("error"):
                    failed_frames[address] += 1
                caller = normalize_address(frame.get("from")) or parent_address
                if caller is not None and depth > 0:
                    call_edges[(caller, address, call_type)] += 1
                    if call_type == "DELEGATECALL":
                        delegatecall_edges[(caller, address)] += 1

    ranked = []
    for address, count in sorted_counter(invocation_count):
        ranked.append(
            {
                "address": address,
                "invocations": count,
                "root_invocations": root_invocation_count[address],
                "internal_invocations": internal_invocation_count[address],
                "transactions_with_invocation": len(tx_ids[address]),
                "failed_frames": failed_frames[address],
                "first_seen_block": first_seen[address],
                "top_selectors": [
                    {"selector": selector, "invocations": n}
                    for selector, n in sorted_counter(selectors[address], 10)
                ],
                "call_types": [
                    {"type": call_type, "invocations": n}
                    for call_type, n in sorted_counter(call_types[address])
                ],
            }
        )

    return {
        "trace_semantics": "Geth built-in callTracer; root plus nested calls; SELFDESTRUCT refund recipients are not treated as invocations",
        "storage_join_note": "callTracer exposes call frames but not per-frame SLOAD/SSTORE ownership. Storage conflict attribution remains keyed by the independently reconstructed storage-owning address; invocation statistics are reported separately.",
        "total_frames": total_frames,
        "internal_frames": internal_frames,
        "unique_invoked_addresses": len(invocation_count),
        "ranked": ranked,
        "first_seen_block": dict(sorted(first_seen.items())),
        "top_call_edges": [
            {"caller": caller, "callee": callee, "type": call_type, "invocations": count}
            for (caller, callee, call_type), count in sorted_counter(call_edges, 250)
        ],
        "delegatecall_edges": [
            {"storage_context_candidate": caller, "implementation_candidate": callee, "invocations": count}
            for (caller, callee), count in sorted_counter(delegatecall_edges)
        ],
    }


def merge_first_seen(*maps: dict[str, int]) -> dict[str, int]:
    result: dict[str, int] = {}
    for mapping in maps:
        for address, block_number in mapping.items():
            address = normalize_address(address)
            if address is None:
                continue
            value = int(block_number)
            result[address] = min(value, result.get(address, value))
    return result


def build_code_targets(characterization: dict, call_summary: dict | None, scope: str) -> dict[str, int]:
    direct = characterization.get("address_first_seen_block", {})
    if scope == "direct":
        return merge_first_seen(direct)
    if scope != "relevant":
        raise ValueError(f"unsupported code scope: {scope}")
    storage = characterization.get("storage_owner_first_seen_block", {})
    calls = (call_summary or {}).get("first_seen_block", {})
    return merge_first_seen(direct, storage, calls)


def _block_conflicts_by_owner(block: dict) -> tuple[set[tuple[int, int]], dict[str, set[tuple[int, int]]]]:
    key_readers: dict[str, set[int]] = defaultdict(set)
    key_writers: dict[str, set[int]] = defaultdict(set)
    for index, tx in enumerate(block.get("transactions") or []):
        for key in set(tx.get("reads", ())):
            key_readers[key].add(index)
        for key in set(tx.get("writes", ())):
            key_writers[key].add(index)
    all_pairs: set[tuple[int, int]] = set()
    owner_pairs: dict[str, set[tuple[int, int]]] = defaultdict(set)
    for key in set(key_readers) | set(key_writers):
        writers = key_writers.get(key, set())
        if not writers:
            continue
        pairs = _conflict_pairs_for_key(key_readers.get(key, set()), writers)
        if not pairs:
            continue
        all_pairs.update(pairs)
        owner = address_from_storage_key(key)
        if owner is not None:
            owner_pairs[owner].update(pairs)
    return all_pairs, owner_pairs


def build_native_port_candidates(
    blocks: list[dict],
    characterization: dict,
    call_blocks: dict[int, dict],
    call_summary: dict,
    code_cache: dict[str, dict],
) -> dict:
    """Rank runtime-code families by the S3 conflict pairs their storage owners explain.

    Conflict coverage is causal at the storage-owner level because it is derived from canonical
    storage keys. callTracer statistics are an independent compositional signal and are not used to
    claim that a particular frame caused a particular slot access.
    """

    relevant_addresses = set(characterization.get("address_first_seen_block", {}))
    relevant_addresses.update(characterization.get("storage_owner_first_seen_block", {}))
    relevant_addresses.update(call_summary.get("first_seen_block", {}))
    family_by_address = code_family_index(code_cache, relevant_addresses)

    family_pairs: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    family_owner_attributions: Counter[str] = Counter()
    owner_pairs_total: Counter[str] = Counter()
    total_unique_pairs: set[tuple[int, int, int]] = set()
    unmapped_pairs: set[tuple[int, int, int]] = set()
    empty_code_pairs: set[tuple[int, int, int]] = set()

    for block in blocks:
        block_number = int(block["block_number"])
        block_pairs, owner_pairs = _block_conflicts_by_owner(block)
        total_unique_pairs.update((block_number, left, right) for left, right in block_pairs)
        for owner, pairs in owner_pairs.items():
            owner_pairs_total[owner] += len(pairs)
            family = family_by_address.get(owner, UNMAPPED_CODE_FAMILY)
            pair_ids = {(block_number, left, right) for left, right in pairs}
            if family == UNMAPPED_CODE_FAMILY:
                unmapped_pairs.update(pair_ids)
            elif family == EMPTY_CODE_FAMILY:
                empty_code_pairs.update(pair_ids)
            else:
                family_pairs[family].update(pair_ids)
                family_owner_attributions[family] += len(pairs)

    family_invocations: Counter[str] = Counter()
    family_internal_invocations: Counter[str] = Counter()
    family_root_invocations: Counter[str] = Counter()
    family_tx_ids: dict[str, set[tuple[int, int]]] = defaultdict(set)
    family_selectors: dict[str, Counter[str]] = defaultdict(Counter)
    family_call_types: dict[str, Counter[str]] = defaultdict(Counter)
    family_invocation_addresses: dict[str, Counter[str]] = defaultdict(Counter)
    delegatecall_family_edges: Counter[tuple[str, str]] = Counter()

    for block in blocks:
        block_number = int(block["block_number"])
        cached = call_blocks.get(block_number)
        if cached is None:
            continue
        for tx_index, traced_tx in enumerate(cached.get("transactions") or []):
            root = traced_tx.get("result") or {}
            for frame, depth, parent_address in iter_call_frames(root):
                call_type = str(frame.get("type") or "UNKNOWN").upper()
                if call_type in SELFDESTRUCT_TYPES:
                    continue
                address = normalize_address(frame.get("to"))
                if address is None:
                    continue
                family = family_by_address.get(address, UNMAPPED_CODE_FAMILY)
                family_invocations[family] += 1
                family_invocation_addresses[family][address] += 1
                family_tx_ids[family].add((block_number, tx_index))
                if depth == 0:
                    family_root_invocations[family] += 1
                else:
                    family_internal_invocations[family] += 1
                selector = selector_from_input(frame.get("input")) if call_type in CALL_TYPES_WITH_INPUT else "0x"
                family_selectors[family][selector] += 1
                family_call_types[family][call_type] += 1
                if call_type == "DELEGATECALL":
                    caller = normalize_address(frame.get("from")) or parent_address
                    if caller is not None:
                        parent_family = family_by_address.get(caller, UNMAPPED_CODE_FAMILY)
                        delegatecall_family_edges[(parent_family, family)] += 1

    direct_entries = {
        item["address"]: item for item in characterization["direct_destinations"]["ranked"]
    }
    storage_entries = {item["address"]: item for item in characterization["storage"]["ranked"]}
    family_addresses: dict[str, set[str]] = defaultdict(set)
    for address, family in family_by_address.items():
        family_addresses[family].add(address)

    family_code_bytes: dict[str, int] = {}
    family_eip1167: dict[str, str | None] = {}
    for address, family in family_by_address.items():
        if family in {EMPTY_CODE_FAMILY, UNMAPPED_CODE_FAMILY}:
            continue
        code_hex = normalize_runtime_code(code_cache[address].get("code"))
        family_code_bytes.setdefault(family, len(code_hex) // 2)
        family_eip1167.setdefault(family, detect_eip1167_implementation(code_hex))

    total_tx = int(characterization["corpus"]["transactions"])
    total_pair_count = len(total_unique_pairs)
    candidates = []
    for family, pairs in family_pairs.items():
        addresses = family_addresses.get(family, set())
        direct_tx = sum(direct_entries.get(address, {}).get("transactions", 0) for address in addresses)
        storage_owner_tx_attributions = sum(
            storage_entries.get(address, {}).get("transactions_touching_storage", 0)
            for address in addresses
        )
        top_owners = sorted(
            (
                {
                    "address": address,
                    "conflict_pairs": owner_pairs_total[address],
                    "transactions_touching_storage": storage_entries.get(address, {}).get(
                        "transactions_touching_storage", 0
                    ),
                    "access_records": storage_entries.get(address, {}).get("access_records", 0),
                }
                for address in addresses
                if owner_pairs_total[address] > 0
            ),
            key=lambda item: (-item["conflict_pairs"], item["address"]),
        )[:10]
        candidates.append(
            {
                "family": family,
                "runtime_code_sha256": family,
                "code_bytes": family_code_bytes.get(family, 0),
                "address_count": len(addresses),
                "unique_conflict_pairs_covered": len(pairs),
                "conflict_pair_coverage": (len(pairs) / total_pair_count) if total_pair_count else 0.0,
                "storage_owner_pair_attributions": family_owner_attributions[family],
                "direct_destination_transactions": direct_tx,
                "direct_tx_coverage": (direct_tx / total_tx) if total_tx else 0.0,
                "storage_owner_transaction_attributions": storage_owner_tx_attributions,
                "invocations": family_invocations[family],
                "root_invocations": family_root_invocations[family],
                "internal_invocations": family_internal_invocations[family],
                "transactions_with_invocation": len(family_tx_ids[family]),
                "invocation_tx_coverage": (len(family_tx_ids[family]) / total_tx) if total_tx else 0.0,
                "eip1167_implementation": family_eip1167.get(family),
                "top_storage_owners": top_owners,
                "top_invocation_addresses": [
                    {"address": address, "invocations": count}
                    for address, count in sorted_counter(family_invocation_addresses[family], 10)
                ],
                "top_selectors": [
                    {"selector": selector, "invocations": count}
                    for selector, count in sorted_counter(family_selectors[family], 10)
                ],
                "call_types": [
                    {"type": call_type, "invocations": count}
                    for call_type, count in sorted_counter(family_call_types[family])
                ],
            }
        )

    candidates.sort(
        key=lambda item: (
            -item["unique_conflict_pairs_covered"],
            -item["transactions_with_invocation"],
            -item["invocations"],
            item["family"],
        )
    )

    cumulative_pairs: set[tuple[int, int, int]] = set()
    cumulative_coverage = []
    points = set(DEFAULT_COVERAGE_POINTS)
    for rank, candidate in enumerate(candidates, start=1):
        cumulative_pairs.update(family_pairs[candidate["family"]])
        if rank in points:
            cumulative_coverage.append(
                {
                    "top_n": rank,
                    "unique_conflict_pairs": len(cumulative_pairs),
                    "coverage": (len(cumulative_pairs) / total_pair_count) if total_pair_count else 0.0,
                }
            )
    for point in DEFAULT_COVERAGE_POINTS:
        if point > len(candidates):
            cumulative_coverage.append(
                {
                    "top_n": point,
                    "unique_conflict_pairs": len(cumulative_pairs),
                    "coverage": (len(cumulative_pairs) / total_pair_count) if total_pair_count else 0.0,
                }
            )
    cumulative_coverage.sort(key=lambda item: item["top_n"])

    return {
        "schema_version": 1,
        "ranking_semantics": "non-empty storage-owner runtime-code families ranked by unique same-block conflict pairs covered; callTracer invocation frequency is reported as a separate feasibility/composition signal and is not used to infer per-frame storage ownership",
        "conflict_definition": characterization["conflicts"]["definition"],
        "total_unique_conflict_pairs": total_pair_count,
        "storage_owner_pair_attributions": characterization["conflicts"]["storage_owner_pair_attributions"],
        "families_with_conflict_coverage": len(candidates),
        "unmapped_code_conflict_pairs": len(unmapped_pairs),
        "empty_code_conflict_pairs": len(empty_code_pairs),
        "cumulative_conflict_coverage": cumulative_coverage,
        "delegatecall_note": "DELEGATECALL edges are exposed as implementation candidates, but storage-owner families are not rewritten to implementation families. General proxy resolution is intentionally deferred.",
        "top_delegatecall_family_edges": [
            {
                "storage_context_family_candidate": parent,
                "implementation_family_candidate": implementation,
                "invocations": count,
            }
            for (parent, implementation), count in sorted_counter(delegatecall_family_edges, 100)
        ],
        "ranked": candidates,
    }



def _family_call_statistics(call_summary: dict, family_by_address: dict[str, str]) -> dict[str, dict]:
    stats: dict[str, dict] = defaultdict(
        lambda: {
            "invocations": 0,
            "root_invocations": 0,
            "internal_invocations": 0,
            "transactions": set(),
            "selectors": Counter(),
            "call_types": Counter(),
            "addresses": Counter(),
        }
    )
    # call_summary intentionally stores only aggregate tx counts per address rather than tx ids.
    # Summing transactions_with_invocation can double count when several instances of one family
    # appear in the same transaction, so family-level transaction coverage is left as an
    # attribution count here and labeled accordingly.
    for item in call_summary.get("ranked", []):
        address = normalize_address(item.get("address"))
        if address is None:
            continue
        family = family_by_address.get(address, UNMAPPED_CODE_FAMILY)
        entry = stats[family]
        entry["invocations"] += int(item.get("invocations", 0))
        entry["root_invocations"] += int(item.get("root_invocations", 0))
        entry["internal_invocations"] += int(item.get("internal_invocations", 0))
        entry["transaction_attributions"] = entry.get("transaction_attributions", 0) + int(
            item.get("transactions_with_invocation", 0)
        )
        entry["addresses"][address] += int(item.get("invocations", 0))
        for selector in item.get("top_selectors", []):
            entry["selectors"][str(selector["selector"])] += int(selector["invocations"])
        for call_type in item.get("call_types", []):
            entry["call_types"][str(call_type["type"])] += int(call_type["invocations"])
    return stats


def heuristic_interface_hints(selector_counts: Counter[str]) -> list[str]:
    selectors = {selector for selector, count in selector_counts.items() if count > 0}
    hints = []
    if KNOWN_INTERFACE_SELECTORS["wrapped-native-token-like"].issubset(selectors):
        hints.append("wrapped-native-token-like")
    if KNOWN_INTERFACE_SELECTORS["constant-product-amm-pair-like"].issubset(selectors):
        hints.append("constant-product-amm-pair-like")
    if (
        ({"0x42842e0e", "0xb88d4fde"} & selectors)
        and "0xa22cb465" in selectors
    ):
        hints.append("nft-like")
    if (
        {"0xa9059cbb", "0x70a08231"}.issubset(selectors)
        and ({"0x23b872dd", "0x095ea7b3", "0xdd62ed3e"} & selectors)
    ):
        hints.append("fungible-token-like")
    return hints


def resolve_storage_owner_profiles(
    characterization: dict,
    call_summary: dict,
    code_cache: dict[str, dict],
    proxy_cache: dict[str, dict],
) -> list[dict]:
    """Resolve storage-owning addresses to the code family that should be analyzed.

    Exact EIP-1167 bytecode and a non-zero EIP-1967 implementation slot are treated as structural
    evidence and may rewrite a storage-context family to an implementation/profile family. Generic
    observed DELEGATECALL edges remain candidates only: an arbitrary contract can delegate to a
    plugin/library, and the storage trace cannot prove which frame caused each access.
    """

    relevant = set(characterization.get("storage_owner_first_seen_block", {}))
    relevant.update(call_summary.get("first_seen_block", {}))
    relevant.update(code_cache)
    family_by_address = code_family_index(code_cache, relevant)
    invocation_by_address = {
        item["address"]: item for item in call_summary.get("ranked", []) if item.get("address")
    }
    delegate_by_owner: dict[str, Counter[str]] = defaultdict(Counter)
    for edge in call_summary.get("delegatecall_edges", []):
        owner = normalize_address(edge.get("storage_context_candidate"))
        implementation = normalize_address(edge.get("implementation_candidate"))
        if owner is None or implementation is None:
            continue
        delegate_by_owner[owner][implementation] += int(edge.get("invocations", 0))

    records = []
    for storage in characterization.get("storage", {}).get("ranked", []):
        conflict_pairs = int(storage.get("conflict_pairs", 0))
        if conflict_pairs <= 0:
            continue
        owner = normalize_address(storage.get("address"))
        if owner is None:
            continue
        owner_family = family_by_address.get(owner, UNMAPPED_CODE_FAMILY)
        owner_code = normalize_runtime_code(code_cache.get(owner, {}).get("code"))
        eip1167_impl = detect_eip1167_implementation(owner_code) if owner_code else None
        eip1967_entry = proxy_cache.get(owner, {})
        eip1967_impl = normalize_address(eip1967_entry.get("implementation_address"))
        beacon = normalize_address(eip1967_entry.get("beacon_address"))

        observed_targets = delegate_by_owner.get(owner, Counter())
        observed_families: Counter[str] = Counter()
        for implementation, count in observed_targets.items():
            observed_families[
                family_by_address.get(implementation, UNMAPPED_CODE_FAMILY)
            ] += count

        resolution_status = "direct-code"
        resolution_source = None
        profile_family = owner_family
        implementation_address = None
        structural_target = None
        if eip1167_impl is not None:
            structural_target = eip1167_impl
            resolution_source = "eip1167-runtime-bytecode"
            resolution_status = "unresolved-eip1167-target-code"
        elif eip1967_impl is not None:
            structural_target = eip1967_impl
            resolution_source = "eip1967-implementation-slot"
            resolution_status = "unresolved-eip1967-target-code"
        elif beacon is not None:
            resolution_source = "eip1967-beacon-slot"
            resolution_status = "unresolved-eip1967-beacon"
        elif observed_targets:
            resolution_source = "callTracer-delegatecall"
            resolution_status = "observed-delegatecall-candidate"

        if structural_target is not None:
            target_family = family_by_address.get(structural_target, UNMAPPED_CODE_FAMILY)
            if target_family not in {UNMAPPED_CODE_FAMILY, EMPTY_CODE_FAMILY}:
                profile_family = target_family
                implementation_address = structural_target
                resolution_status = (
                    "resolved-eip1167" if eip1167_impl is not None else "resolved-eip1967"
                )

        invocation_entry = invocation_by_address.get(owner, {})
        records.append(
            {
                "storage_owner": owner,
                "storage_context_family": owner_family,
                "recommended_profile_family": profile_family,
                "resolution_status": resolution_status,
                "resolution_source": resolution_source,
                "implementation_address": implementation_address,
                "eip1167_implementation": eip1167_impl,
                "eip1967_implementation": eip1967_impl,
                "eip1967_beacon": beacon,
                "owner_invocations": int(invocation_entry.get("invocations", 0)),
                "delegatecall_invocations": sum(observed_targets.values()),
                "observed_delegatecall_targets": [
                    {
                        "address": address,
                        "family": family_by_address.get(address, UNMAPPED_CODE_FAMILY),
                        "invocations": count,
                    }
                    for address, count in sorted_counter(observed_targets, 20)
                ],
                "observed_delegatecall_families": [
                    {"family": family, "invocations": count}
                    for family, count in sorted_counter(observed_families, 20)
                ],
                "conflict_pairs": conflict_pairs,
                "transactions_touching_storage": int(
                    storage.get("transactions_touching_storage", 0)
                ),
                "access_records": int(storage.get("access_records", 0)),
            }
        )
    records.sort(key=lambda item: (-item["conflict_pairs"], item["storage_owner"]))
    return records


def build_native_family_mapping_candidates(
    blocks: list[dict],
    characterization: dict,
    call_summary: dict,
    code_cache: dict[str, dict],
    proxy_cache: dict[str, dict],
) -> dict:
    """Group conflict coverage by the recommended source-analysis/profile family.

    Storage ownership never changes: proxy instances remain distinct storage namespaces. Only the
    family whose source/profile should describe those accesses is rewritten when structural proxy
    evidence identifies an implementation.
    """

    resolutions = resolve_storage_owner_profiles(
        characterization, call_summary, code_cache, proxy_cache
    )
    resolution_by_owner = {item["storage_owner"]: item for item in resolutions}

    relevant = set(code_cache)
    family_by_address = code_family_index(code_cache, relevant)
    call_stats = _family_call_statistics(call_summary, family_by_address)

    total_pairs: set[tuple[int, int, int]] = set()
    profile_pairs: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    profile_owner_attributions: Counter[str] = Counter()
    profile_owners: dict[str, set[str]] = defaultdict(set)
    profile_storage_families: dict[str, set[str]] = defaultdict(set)
    status_pairs: dict[str, set[tuple[int, int, int]]] = defaultdict(set)
    status_owner_counts: Counter[str] = Counter()

    for block in blocks:
        block_number = int(block["block_number"])
        block_pairs, owner_pairs = _block_conflicts_by_owner(block)
        total_pairs.update((block_number, left, right) for left, right in block_pairs)
        for owner, pairs in owner_pairs.items():
            resolution = resolution_by_owner.get(owner)
            if resolution is None:
                continue
            profile_family = resolution["recommended_profile_family"]
            ids = {(block_number, left, right) for left, right in pairs}
            profile_pairs[profile_family].update(ids)
            profile_owner_attributions[profile_family] += len(pairs)
            profile_owners[profile_family].add(owner)
            profile_storage_families[profile_family].add(
                resolution["storage_context_family"]
            )
            status_pairs[resolution["resolution_status"]].update(ids)

    for resolution in resolutions:
        status_owner_counts[resolution["resolution_status"]] += 1

    total_pair_count = len(total_pairs)
    ranked = []
    for profile_family, pairs in profile_pairs.items():
        owners = profile_owners[profile_family]
        owner_records = [resolution_by_owner[owner] for owner in owners]
        stats = call_stats.get(profile_family, {})
        selector_counts = stats.get("selectors", Counter())
        ranked.append(
            {
                "profile_family": profile_family,
                "unique_conflict_pairs_covered": len(pairs),
                "conflict_pair_coverage": (len(pairs) / total_pair_count) if total_pair_count else 0.0,
                "storage_owner_pair_attributions": profile_owner_attributions[profile_family],
                "storage_owner_count": len(owners),
                "storage_context_families": sorted(profile_storage_families[profile_family]),
                "resolved_proxy_owner_count": sum(
                    1 for item in owner_records if item["resolution_status"].startswith("resolved-")
                ),
                "unresolved_delegatecall_owner_count": sum(
                    1
                    for item in owner_records
                    if item["resolution_status"]
                    in {"observed-delegatecall-candidate", "unresolved-eip1967-beacon"}
                ),
                "invocations": int(stats.get("invocations", 0)),
                "root_invocations": int(stats.get("root_invocations", 0)),
                "internal_invocations": int(stats.get("internal_invocations", 0)),
                "invocation_transaction_attributions": int(
                    stats.get("transaction_attributions", 0)
                ),
                "heuristic_interface_hints": heuristic_interface_hints(selector_counts),
                "top_selectors": [
                    {"selector": selector, "invocations": count}
                    for selector, count in sorted_counter(selector_counts, 10)
                ],
                "top_storage_owners": [
                    {
                        "address": item["storage_owner"],
                        "conflict_pairs": item["conflict_pairs"],
                        "resolution_status": item["resolution_status"],
                        "storage_context_family": item["storage_context_family"],
                        "implementation_address": item["implementation_address"],
                    }
                    for item in sorted(
                        owner_records,
                        key=lambda item: (-item["conflict_pairs"], item["storage_owner"]),
                    )[:20]
                ],
            }
        )

    ranked.sort(
        key=lambda item: (
            -item["unique_conflict_pairs_covered"],
            -item["invocations"],
            item["profile_family"],
        )
    )

    cumulative_pairs: set[tuple[int, int, int]] = set()
    cumulative = []
    target_points = set(DEFAULT_COVERAGE_POINTS)
    target_thresholds = (0.90, 0.95, 0.98)
    threshold_results: dict[float, dict | None] = {value: None for value in target_thresholds}
    for rank, item in enumerate(ranked, start=1):
        cumulative_pairs.update(profile_pairs[item["profile_family"]])
        coverage = (len(cumulative_pairs) / total_pair_count) if total_pair_count else 0.0
        if rank in target_points:
            cumulative.append(
                {"top_n": rank, "unique_conflict_pairs": len(cumulative_pairs), "coverage": coverage}
            )
        for threshold in target_thresholds:
            if threshold_results[threshold] is None and coverage >= threshold:
                threshold_results[threshold] = {
                    "target_coverage": threshold,
                    "minimum_profile_families": rank,
                    "achieved_coverage": coverage,
                    "unique_conflict_pairs": len(cumulative_pairs),
                }
    for point in DEFAULT_COVERAGE_POINTS:
        if point > len(ranked):
            cumulative.append(
                {
                    "top_n": point,
                    "unique_conflict_pairs": len(cumulative_pairs),
                    "coverage": (len(cumulative_pairs) / total_pair_count) if total_pair_count else 0.0,
                }
            )
    cumulative.sort(key=lambda item: item["top_n"])

    resolution_summary = []
    for status, owner_count in sorted(status_owner_counts.items()):
        pairs = status_pairs.get(status, set())
        resolution_summary.append(
            {
                "status": status,
                "storage_owner_count": owner_count,
                "unique_conflict_pairs": len(pairs),
                "conflict_pair_coverage": (len(pairs) / total_pair_count) if total_pair_count else 0.0,
            }
        )

    return {
        "schema_version": 1,
        "mapping_semantics": "storage owners remain distinct namespaces; exact EIP-1167 runtime structure and non-zero EIP-1967 implementation slots may rewrite the source-analysis/profile family to the implementation runtime-code family; generic callTracer DELEGATECALL edges remain unresolved candidates",
        "conflict_definition": characterization["conflicts"]["definition"],
        "total_unique_conflict_pairs": total_pair_count,
        "profile_families_with_conflict_coverage": len(ranked),
        "cumulative_conflict_coverage": cumulative,
        "coverage_targets": [
            threshold_results[threshold]
            or {
                "target_coverage": threshold,
                "minimum_profile_families": None,
                "achieved_coverage": (len(cumulative_pairs) / total_pair_count) if total_pair_count else 0.0,
                "unique_conflict_pairs": len(cumulative_pairs),
            }
            for threshold in target_thresholds
        ],
        "resolution_summary": resolution_summary,
        "resolution_records": resolutions,
        "interface_hint_note": "interface hints are selector-set heuristics for native-port triage only; they are not contract identity claims and do not replace source-derived symbolic analysis",
        "ranked": ranked,
    }


def render_native_family_mapping_text(report: dict, top: int) -> str:
    lines = [
        "Vegeta S3 proxy-resolved native family mapping candidates",
        "",
        report["mapping_semantics"],
        "",
        f"unique conflict pairs: {report['total_unique_conflict_pairs']}",
        f"profile families with conflict coverage: {report['profile_families_with_conflict_coverage']}",
        "",
        "Resolution summary:",
    ]
    for item in report["resolution_summary"]:
        lines.append(
            f"  {item['status']:<42} owners={item['storage_owner_count']:<5} "
            f"pairs={item['unique_conflict_pairs']:<5} coverage={item['conflict_pair_coverage'] * 100:6.2f}%"
        )
    lines.extend(["", "Cumulative profile-family conflict coverage:"])
    for item in report["cumulative_conflict_coverage"]:
        lines.append(
            f"  top {item['top_n']:>3}: {item['coverage'] * 100:6.2f}% "
            f"({item['unique_conflict_pairs']} pairs)"
        )
    lines.extend(["", "Coverage targets:"])
    for item in report["coverage_targets"]:
        rank = item["minimum_profile_families"]
        rank_text = str(rank) if rank is not None else "not reached"
        lines.append(
            f"  {item['target_coverage'] * 100:5.1f}% target: {rank_text} families "
            f"(achieved {item['achieved_coverage'] * 100:6.2f}%)"
        )
    lines.extend(["", f"Top {top} mapped profile families:"])
    for rank, item in enumerate(report["ranked"][:top], start=1):
        hints = ",".join(item["heuristic_interface_hints"]) or "-"
        top_owner = item["top_storage_owners"][0]["address"] if item["top_storage_owners"] else "-"
        lines.append(
            f"  {rank:>3}. profile={item['profile_family']} conflicts={item['unique_conflict_pairs_covered']:<5} "
            f"coverage={item['conflict_pair_coverage'] * 100:6.2f}% owners={item['storage_owner_count']:<4} "
            f"resolved_proxies={item['resolved_proxy_owner_count']:<3} hints={hints} top_owner={top_owner}"
        )
    lines.extend(
        [
            "",
            "Interpretation notes:",
            "  * proxy resolution changes the recommended code/profile family, never the storage namespace;",
            "  * exact EIP-1167 and EIP-1967 implementation-slot evidence can resolve a profile family;",
            "  * generic DELEGATECALL edges remain candidates because callTracer cannot attribute each storage access to a frame;",
            "  * interface hints are triage heuristics only; final native mappings require real contract/source inspection and LLM symbolic analyses.",
        ]
    )
    return "\n".join(lines) + "\n"

def render_native_port_candidates_text(report: dict, top: int) -> str:
    lines = [
        "Vegeta S3 conflict-weighted native-port candidates",
        "",
        report["ranking_semantics"],
        "",
        f"unique conflict pairs: {report['total_unique_conflict_pairs']}",
        f"storage-owner pair attributions: {report['storage_owner_pair_attributions']}",
        f"families with conflict coverage: {report['families_with_conflict_coverage']}",
        f"unmapped-code conflict pairs: {report['unmapped_code_conflict_pairs']}",
        f"empty-code conflict pairs: {report['empty_code_conflict_pairs']}",
        "",
        "Cumulative unique conflict-pair coverage:",
    ]
    for item in report["cumulative_conflict_coverage"]:
        lines.append(
            f"  top {item['top_n']:>3}: {item['coverage'] * 100:6.2f}% "
            f"({item['unique_conflict_pairs']} pairs)"
        )
    lines.extend(["", f"Top {top} candidate code families:"])
    for rank, item in enumerate(report["ranked"][:top], start=1):
        top_owner = item["top_storage_owners"][0]["address"] if item["top_storage_owners"] else "-"
        top_selector = item["top_selectors"][0]["selector"] if item["top_selectors"] else "-"
        lines.append(
            f"  {rank:>3}. sha256={item['family']} conflicts={item['unique_conflict_pairs_covered']:<5} "
            f"coverage={item['conflict_pair_coverage'] * 100:6.2f}% "
            f"inv_tx={item['transactions_with_invocation']:<5} internal_calls={item['internal_invocations']:<6} "
            f"direct_tx={item['direct_destination_transactions']:<5} owner={top_owner} selector={top_selector}"
        )
    lines.extend(
        [
            "",
            "Interpretation notes:",
            "  * conflict coverage is attributed from canonical storage keys to storage-owning addresses, then grouped by runtime bytecode hash;",
            "  * callTracer invocations are compositional evidence only and are not a per-frame SLOAD/SSTORE attribution;",
            "  * DELEGATECALL/proxy implementation resolution is not applied yet; use the delegatecall edges as the next diagnostic.",
        ]
    )
    return "\n".join(lines) + "\n"

def build_code_families(characterization: dict, code_cache: dict[str, dict]) -> dict:
    destination_entries = {
        item["address"]: item for item in characterization["direct_destinations"]["ranked"]
    }
    total_transactions = characterization["corpus"]["transactions"]
    family_members: dict[str, list[str]] = defaultdict(list)
    family_code: dict[str, str] = {}
    empty_addresses = 0

    for address, entry in sorted(code_cache.items()):
        if address not in destination_entries:
            continue
        code_hex = normalize_runtime_code(entry.get("code"))
        if not code_hex:
            family_id = EMPTY_CODE_FAMILY
            empty_addresses += 1
        else:
            family_id = runtime_code_sha256(code_hex)
            family_code[family_id] = code_hex
        family_members[family_id].append(address)

    families = []
    non_empty_family_counter: Counter[str] = Counter()
    for family_id, addresses in family_members.items():
        tx_count = sum(destination_entries[address]["transactions"] for address in addresses)
        selector_counter: Counter[str] = Counter()
        address_set = set(addresses)
        for method in characterization["methods"]["ranked"]:
            if method["address"] in address_set:
                selector_counter[method["selector"]] += method["transactions"]
        code_hex = family_code.get(family_id, "")
        if family_id != EMPTY_CODE_FAMILY:
            non_empty_family_counter[family_id] = tx_count
        families.append(
            {
                "family": family_id,
                "runtime_code_sha256": None if family_id == EMPTY_CODE_FAMILY else family_id,
                "code_bytes": len(code_hex) // 2,
                "address_count": len(addresses),
                "transactions": tx_count,
                "tx_coverage": (tx_count / total_transactions) if total_transactions else 0.0,
                "eip1167_implementation": detect_eip1167_implementation(code_hex) if code_hex else None,
                "top_addresses": [
                    {
                        "address": address,
                        "transactions": destination_entries[address]["transactions"],
                    }
                    for address in sorted(
                        addresses,
                        key=lambda address: (-destination_entries[address]["transactions"], address),
                    )[:10]
                ],
                "top_selectors": [
                    {"selector": selector, "transactions": count}
                    for selector, count in sorted_counter(selector_counter, 10)
                ],
            }
        )

    families.sort(key=lambda item: (-item["transactions"], item["family"]))
    contract_destination_transactions = sum(non_empty_family_counter.values())
    return {
        "code_snapshot_semantics": "eth_getCode at each destination's first-seen block unless --code-block overrides it",
        "proxy_note": "runtime-bytecode families do not resolve EIP-1967/other delegatecall implementation storage; EIP-1167 minimal proxies are detected when bytecode matches the canonical runtime form",
        "addresses_with_code_records": sum(len(v) for v in family_members.values()),
        "empty_code_addresses": empty_addresses,
        "non_empty_code_addresses": sum(
            len(addresses)
            for family_id, addresses in family_members.items()
            if family_id != EMPTY_CODE_FAMILY
        ),
        "unique_non_empty_code_families": len(non_empty_family_counter),
        "contract_destination_transactions": contract_destination_transactions,
        "coverage": coverage_curve(non_empty_family_counter, contract_destination_transactions),
        "ranked": families,
    }


def render_text(report: dict, top: int) -> str:
    corpus = report["corpus"]
    lines = [
        "Vegeta Ethereum corpus contract-family characterization",
        "",
        f"blocks: {corpus['blocks']} ({corpus['first_block']}..{corpus['last_block']})",
        f"transactions: {corpus['transactions']}",
        f"failed transactions: {corpus['failed_transactions']}",
        f"contract creations: {corpus['contract_creations']}",
        f"unique direct destinations: {report['direct_destinations']['unique_addresses']}",
        f"unique selectors: {report['selectors']['unique_selectors']}",
        f"unique (destination, selector) methods: {report['methods']['unique_destination_selector_pairs']}",
        f"unique storage owners: {report['storage']['unique_storage_owners']}",
        f"same-block conflict pairs: {report['conflicts']['total_unique_tx_pairs']}",
        "",
        "Direct destination coverage:",
    ]
    for item in report["direct_destinations"]["coverage"]:
        lines.append(f"  top {item['top_n']:>3}: {item['coverage'] * 100:6.2f}% ({item['count']} tx)")

    lines.extend(["", f"Top {top} direct destinations:"])
    for rank, item in enumerate(report["direct_destinations"]["ranked"][:top], start=1):
        selector = item["top_selectors"][0]["selector"] if item["top_selectors"] else "-"
        lines.append(
            f"  {rank:>3}. {item['address']} tx={item['transactions']:<5} "
            f"coverage={item['tx_coverage'] * 100:6.2f}% selectors={item['unique_selectors']:<4} "
            f"top_selector={selector}"
        )

    lines.extend(["", f"Top {top} methods:"])
    for rank, item in enumerate(report["methods"]["ranked"][:top], start=1):
        lines.append(
            f"  {rank:>3}. {item['address']} {item['selector']} "
            f"tx={item['transactions']:<5} coverage={item['tx_coverage'] * 100:6.2f}%"
        )

    lines.extend(["", f"Top {top} storage owners:"])
    for rank, item in enumerate(report["storage"]["ranked"][:top], start=1):
        lines.append(
            f"  {rank:>3}. {item['address']} tx={item['transactions_touching_storage']:<5} "
            f"accesses={item['access_records']:<7} keys={item['unique_storage_keys']:<6} "
            f"conflict_pairs={item['conflict_pairs']}"
        )

    code = report.get("code_families")
    if code is not None:
        lines.extend(
            [
                "",
                "Runtime-bytecode families:",
                f"  non-empty code addresses: {code['non_empty_code_addresses']}",
                f"  empty-code addresses: {code['empty_code_addresses']}",
                f"  unique non-empty families: {code['unique_non_empty_code_families']}",
            ]
        )
        for item in code["coverage"]:
            lines.append(
                f"  top {item['top_n']:>3} code families: "
                f"{item['coverage'] * 100:6.2f}% ({item['count']} contract-destination tx)"
            )
        lines.extend(["", f"Top {top} code families:"])
        visible = [item for item in code["ranked"] if item["family"] != EMPTY_CODE_FAMILY][:top]
        for rank, item in enumerate(visible, start=1):
            lines.append(
                f"  {rank:>3}. sha256={item['runtime_code_sha256']} bytes={item['code_bytes']:<6} "
                f"addresses={item['address_count']:<4} tx={item['transactions']:<5} "
                f"coverage={item['tx_coverage'] * 100:6.2f}%"
            )
        lines.append("")
        lines.append("Note: bytecode-family grouping does not yet resolve general proxy/delegatecall implementations.")

    calls = report.get("call_traces")
    if calls is not None:
        lines.extend(
            [
                "",
                "callTracer composition:",
                f"  total call frames: {calls['total_frames']}",
                f"  internal call frames: {calls['internal_frames']}",
                f"  unique invoked addresses: {calls['unique_invoked_addresses']}",
                "",
                f"Top {top} invoked addresses:",
            ]
        )
        for rank, item in enumerate(calls["ranked"][:top], start=1):
            selector = item["top_selectors"][0]["selector"] if item["top_selectors"] else "-"
            lines.append(
                f"  {rank:>3}. {item['address']} invocations={item['invocations']:<6} "
                f"tx={item['transactions_with_invocation']:<5} internal={item['internal_invocations']:<6} "
                f"top_selector={selector}"
            )
        lines.append("")
        lines.append("Note: callTracer frames are not used as per-frame storage-access attribution.")

    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path, help="Vegeta corpus.jsonl")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=None,
        help="output directory (default: <corpus-dir>/characterization)",
    )
    parser.add_argument("--top", type=int, default=25, help="rows shown in the text summary")
    parser.add_argument(
        "--fetch-calls",
        action="store_true",
        help="fetch resumable Geth callTracer trees for all corpus blocks",
    )
    parser.add_argument(
        "--fetch-code",
        action="store_true",
        help="fetch historical runtime bytecode and group by SHA-256",
    )
    parser.add_argument(
        "--code-scope",
        choices=("direct", "relevant"),
        default="direct",
        help="addresses whose code is fetched: direct destinations only, or direct + storage owners + callTracer callees",
    )
    parser.add_argument(
        "--native-port-candidates",
        action="store_true",
        help="emit conflict-weighted native-port candidate report; requires --fetch-calls and --fetch-code and uses relevant code scope",
    )
    parser.add_argument(
        "--native-family-mapping-candidates",
        action="store_true",
        help="probe standard proxy slots and emit proxy-resolved source-analysis/profile-family candidates; implies --native-port-candidates",
    )
    parser.add_argument(
        "--rpc-url",
        default=os.environ.get("ETH_RPC_URL"),
        help="Ethereum JSON-RPC URL (default: ETH_RPC_URL)",
    )
    parser.add_argument(
        "--code-block",
        type=int,
        default=None,
        help="use one historical block for every eth_getCode call; default is each address's first-seen relevant block",
    )
    parser.add_argument(
        "--call-trace-timeout",
        type=int,
        default=600,
        help="Geth tracer timeout string in seconds for each block-tracing request",
    )
    parser.add_argument(
        "--call-trace-reexec",
        type=int,
        default=128,
        help="Geth historical-state reexec allowance for callTracer",
    )
    parser.add_argument("--rpc-timeout", type=int, default=600)
    parser.add_argument("--rpc-retries", type=int, default=5)
    parser.add_argument("--rpc-backoff", type=float, default=1.0)
    parser.add_argument(
        "--rpc-delay-ms",
        type=int,
        default=50,
        help="delay between eth_getCode calls to reduce public-provider throttling",
    )
    parser.add_argument(
        "--call-delay-ms",
        type=int,
        default=100,
        help="delay between uncached block callTracer requests",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.top <= 0:
        raise SystemExit("--top must be positive")
    if args.call_trace_timeout <= 0:
        raise SystemExit("--call-trace-timeout must be positive")
    if args.call_trace_reexec < 0:
        raise SystemExit("--call-trace-reexec must be non-negative")
    if args.native_family_mapping_candidates:
        args.native_port_candidates = True
    if args.native_port_candidates:
        if not args.fetch_calls or not args.fetch_code:
            flag = "--native-family-mapping-candidates" if args.native_family_mapping_candidates else "--native-port-candidates"
            raise SystemExit(f"{flag} requires both --fetch-calls and --fetch-code")
        args.code_scope = "relevant"
    if args.code_scope == "relevant" and not args.fetch_calls:
        # Storage owners are still relevant without callTracer, but the requested native-port
        # characterization specifically needs internal callees. Avoid a silently partial report.
        if args.native_port_candidates:
            raise SystemExit("relevant native-port code scope requires --fetch-calls")

    blocks = load_blocks(args.corpus)
    if not blocks:
        raise SystemExit(f"empty corpus: {args.corpus}")

    output_dir = args.output_dir or (args.corpus.parent / "characterization")
    output_dir.mkdir(parents=True, exist_ok=True)

    report = characterize_blocks(blocks)
    client = None
    if args.fetch_calls or args.fetch_code:
        if not args.rpc_url:
            raise SystemExit("RPC-backed characterization requires --rpc-url or ETH_RPC_URL")
        client = RpcClient(
            args.rpc_url,
            timeout=args.rpc_timeout,
            retries=args.rpc_retries,
            backoff=args.rpc_backoff,
        )

    call_blocks: dict[int, dict] = {}
    call_summary = None
    if args.fetch_calls:
        assert client is not None
        call_cache_dir = output_dir / "call-cache"
        call_blocks = fetch_call_traces(
            blocks,
            client,
            call_cache_dir,
            trace_timeout_seconds=args.call_trace_timeout,
            reexec=args.call_trace_reexec,
            delay_ms=args.call_delay_ms,
        )
        call_summary = summarize_call_traces(blocks, call_blocks)
        report["call_traces"] = call_summary

    code_cache: dict[str, dict] = {}
    if args.fetch_code:
        assert client is not None
        cache_path = output_dir / "code-cache.json"
        targets = build_code_targets(report, call_summary, args.code_scope)
        code_cache = fetch_runtime_codes(
            targets,
            client,
            cache_path,
            fixed_block=args.code_block,
            delay_ms=args.rpc_delay_ms,
        )
        # Preserve the original destination-family view for continuity with the first
        # characterization pass. The cache may now contain internal callees/storage owners too.
        report["code_families"] = build_code_families(report, code_cache)
        report["code_fetch"] = {
            "scope": args.code_scope,
            "target_addresses": len(targets),
            "snapshot_semantics": "eth_getCode at each address's first-seen relevant block unless --code-block overrides it",
        }

    native_candidates = None
    if args.native_port_candidates:
        assert call_summary is not None
        native_candidates = build_native_port_candidates(
            blocks,
            report,
            call_blocks,
            call_summary,
            code_cache,
        )
        report["native_port_candidates"] = native_candidates

    native_family_mapping = None
    proxy_cache: dict[str, dict] = {}
    if args.native_family_mapping_candidates:
        assert client is not None
        assert call_summary is not None
        proxy_cache_path = output_dir / "proxy-resolution-cache.json"
        proxy_targets = proxy_probe_targets(report)
        proxy_cache = fetch_eip1967_slots(
            proxy_targets,
            client,
            proxy_cache_path,
            fixed_block=args.code_block,
            delay_ms=args.rpc_delay_ms,
        )
        implementation_targets = proxy_implementation_code_targets(
            proxy_cache, report, code_cache
        )
        if implementation_targets:
            code_cache = fetch_runtime_codes(
                implementation_targets,
                client,
                output_dir / "code-cache.json",
                fixed_block=args.code_block,
                delay_ms=args.rpc_delay_ms,
            )
        native_family_mapping = build_native_family_mapping_candidates(
            blocks,
            report,
            call_summary,
            code_cache,
            proxy_cache,
        )
        report["native_family_mapping_candidates"] = native_family_mapping
        report["proxy_resolution"] = {
            "probed_conflict_storage_owners": len(proxy_targets),
            "cache": "proxy-resolution-cache.json",
            "eip1967_implementation_slot": EIP1967_IMPLEMENTATION_SLOT,
            "eip1967_beacon_slot": EIP1967_BEACON_SLOT,
        }

    # First-seen maps are needed while fetching/joining but are noisy in the final report; the
    # ranked destination/storage/call entries retain their per-address first-seen blocks.
    report.pop("address_first_seen_block", None)
    report.pop("storage_owner_first_seen_block", None)
    if report.get("call_traces") is not None:
        report["call_traces"].pop("first_seen_block", None)

    report_path = output_dir / "characterization.json"
    text_path = output_dir / "characterization.txt"
    write_json_atomic(report_path, report)
    text_path.write_text(render_text(report, args.top), encoding="utf-8")

    print(render_text(report, args.top), end="")
    print(f"wrote {report_path}")
    print(f"wrote {text_path}")
    if args.fetch_calls:
        print(f"call cache: {output_dir / 'call-cache'}")
    if args.fetch_code:
        print(f"code cache: {output_dir / 'code-cache.json'}")
    if native_candidates is not None:
        candidate_json = output_dir / "native-port-candidates.json"
        candidate_text = output_dir / "native-port-candidates.txt"
        write_json_atomic(candidate_json, native_candidates)
        candidate_text.write_text(
            render_native_port_candidates_text(native_candidates, args.top), encoding="utf-8"
        )
        print(render_native_port_candidates_text(native_candidates, args.top), end="")
        print(f"wrote {candidate_json}")
        print(f"wrote {candidate_text}")
    if native_family_mapping is not None:
        mapping_json = output_dir / "native-family-mapping-candidates.json"
        mapping_text = output_dir / "native-family-mapping-candidates.txt"
        write_json_atomic(mapping_json, native_family_mapping)
        mapping_text.write_text(
            render_native_family_mapping_text(native_family_mapping, args.top), encoding="utf-8"
        )
        print(render_native_family_mapping_text(native_family_mapping, args.top), end="")
        print(f"proxy cache: {output_dir / 'proxy-resolution-cache.json'}")
        print(f"wrote {mapping_json}")
        print(f"wrote {mapping_text}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
