#!/usr/bin/env python3
"""Shared corpus schema and validation helpers for the Vegeta Ethereum workload port."""

from __future__ import annotations

import json
from collections import Counter
from pathlib import Path
from typing import Iterable

SCHEMA_VERSION = 1
S3_START_BLOCK = 16_774_645
S3_END_BLOCK = 16_774_745
S3_EXPECTED_BLOCKS = 101
# Canonical Ethereum mainnet reconstruction for the stated S3 range.
S3_CANONICAL_TRANSACTIONS = 13_783
# Vegeta NSDI'25 Table 2 reports 15,129 transactions for the same stated range.
# Keep this as paper metadata rather than using it as the corpus identity check.
S3_EXPECTED_TRANSACTIONS = 15_129
S3_EXPECTED_LONGEST_CHAIN_SUM = 1_779
S3_EXPECTED_RATIO = 8.50
WETH_MAINNET = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"


def load_blocks(path: Path) -> list[dict]:
    blocks: list[dict] = []
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError(f"{path}:{line_number}: block record must be a JSON object")
            blocks.append(value)
    return blocks


def write_jsonl(path: Path, blocks: Iterable[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for block in blocks:
            handle.write(json.dumps(block, sort_keys=True, separators=(",", ":")))
            handle.write("\n")


def canonical_accesses(tx: dict) -> set[str]:
    return set(tx.get("reads", ())) | set(tx.get("writes", ()))


def storage_contract(key: str) -> str | None:
    # Canonical extractor key: evm/<20-byte-address>/<32-byte-slot>
    parts = key.split("/")
    if len(parts) != 3 or parts[0] != "evm" or len(parts[1]) != 40:
        return None
    return parts[1].lower()


def compute_metrics(blocks: list[dict]) -> dict:
    tx_count = 0
    longest_chain_sum = 0
    max_chain = 0
    max_chain_key = None
    contract_transactions: Counter[str] = Counter()
    contract_accesses: Counter[str] = Counter()
    longest_chain_contract_contribution: Counter[str] = Counter()

    for block in blocks:
        per_key: Counter[str] = Counter()
        for tx in block.get("transactions", []):
            tx_count += 1
            accesses = canonical_accesses(tx)
            for key in accesses:
                per_key[key] += 1
            touched_contracts = {storage_contract(key) for key in accesses}
            touched_contracts.discard(None)
            for contract in touched_contracts:
                contract_transactions[contract] += 1
            for key in tx.get("reads", []):
                contract = storage_contract(key)
                if contract:
                    contract_accesses[contract] += 1
            for key in tx.get("writes", []):
                contract = storage_contract(key)
                if contract:
                    contract_accesses[contract] += 1

        if per_key:
            block_key, block_chain = max(per_key.items(), key=lambda item: (item[1], item[0]))
            longest_chain_sum += block_chain
            block_contract = storage_contract(block_key)
            if block_contract:
                longest_chain_contract_contribution[block_contract] += block_chain
            if block_chain > max_chain:
                max_chain = block_chain
                max_chain_key = block_key

    ratio = (tx_count / longest_chain_sum) if longest_chain_sum else None
    hot_contract = contract_transactions.most_common(1)[0][0] if contract_transactions else None
    chain_contract = (
        longest_chain_contract_contribution.most_common(1)[0][0]
        if longest_chain_contract_contribution
        else None
    )
    return {
        "blocks": len(blocks),
        "transactions": tx_count,
        "longest_chain_sum": longest_chain_sum,
        "ratio": ratio,
        "max_single_block_chain": max_chain,
        "max_single_block_chain_key": max_chain_key,
        "hot_contract_by_transactions": hot_contract,
        "hot_contract_transaction_count": contract_transactions.get(hot_contract, 0)
        if hot_contract
        else 0,
        "dominant_longest_chain_contract": chain_contract,
        "dominant_longest_chain_contribution": longest_chain_contract_contribution.get(
            chain_contract, 0
        )
        if chain_contract
        else 0,
        "weth_longest_chain_contribution": longest_chain_contract_contribution.get(
            WETH_MAINNET, 0
        ),
        "weth_transaction_count": contract_transactions.get(WETH_MAINNET, 0),
        "weth_access_count": contract_accesses.get(WETH_MAINNET, 0),
    }


def validate_shape(blocks: list[dict], start_block: int, end_block: int) -> list[str]:
    errors: list[str] = []
    expected_numbers = list(range(start_block, end_block + 1))
    numbers = [int(block.get("block_number", -1)) for block in blocks]
    if numbers != expected_numbers:
        errors.append(
            f"block numbers are not the exact contiguous range {start_block}..{end_block}"
        )
    for block in blocks:
        transactions = block.get("transactions")
        if not isinstance(transactions, list):
            errors.append(f"block {block.get('block_number')} transactions must be a list")
            continue
        for expected_index, tx in enumerate(transactions):
            if tx.get("tx_index") != expected_index:
                errors.append(
                    f"block {block.get('block_number')} tx index mismatch: "
                    f"expected {expected_index}, got {tx.get('tx_index')}"
                )
            for field in ("tx_hash", "from", "selector", "reads", "writes"):
                if field not in tx:
                    errors.append(
                        f"block {block.get('block_number')} tx {expected_index} missing {field}"
                    )
    return errors
