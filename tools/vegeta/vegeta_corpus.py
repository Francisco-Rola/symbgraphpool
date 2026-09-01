#!/usr/bin/env python3
"""Shared corpus schema and validation helpers for the Vegeta Ethereum workload port."""

from __future__ import annotations

import json
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

SCHEMA_VERSION = 1
WETH_MAINNET = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"


@dataclass(frozen=True)
class VegetaDatasetSpec:
    tag: str
    start_block: int
    end_block: int
    paper_transactions: int
    paper_longest_chain_sum: int
    paper_ratio: float
    # A canonical count is only recorded after independently reconstructing the
    # stated Ethereum mainnet range. S3 is currently the only frozen range with
    # that audit in this repository.
    canonical_transactions: int | None = None

    @property
    def blocks(self) -> int:
        return self.end_block - self.start_block + 1


# Vegeta NSDI'25 Table 2. Keep these ranges centralized so collection scripts,
# manifests, and validators cannot silently drift from the paper definition.
VEGETA_DATASETS: dict[str, VegetaDatasetSpec] = {
    "S1": VegetaDatasetSpec(
        tag="S1",
        start_block=16_774_645,
        end_block=16_779_644,
        paper_transactions=739_863,
        paper_longest_chain_sum=88_136,
        paper_ratio=8.39,
    ),
    "S2": VegetaDatasetSpec(
        tag="S2",
        start_block=16_774_645,
        end_block=16_777_644,
        paper_transactions=436_115,
        paper_longest_chain_sum=52_862,
        paper_ratio=8.25,
    ),
    "S3": VegetaDatasetSpec(
        tag="S3",
        start_block=16_774_645,
        end_block=16_774_745,
        paper_transactions=15_129,
        paper_longest_chain_sum=1_779,
        paper_ratio=8.50,
        canonical_transactions=13_783,
    ),
    "S4": VegetaDatasetSpec(
        tag="S4",
        start_block=18_581_726,
        end_block=18_586_725,
        paper_transactions=747_651,
        paper_longest_chain_sum=89_961,
        paper_ratio=8.31,
    ),
}


def dataset_by_tag(tag: str) -> VegetaDatasetSpec:
    try:
        return VEGETA_DATASETS[tag.upper()]
    except KeyError as error:
        raise ValueError(f"unknown Vegeta dataset tag {tag!r}") from error


def dataset_for_range(start_block: int, end_block: int) -> VegetaDatasetSpec | None:
    for spec in VEGETA_DATASETS.values():
        if (spec.start_block, spec.end_block) == (start_block, end_block):
            return spec
    return None


# Backward-compatible S3 names used throughout the existing S3 toolchain.
_S3 = VEGETA_DATASETS["S3"]
S3_START_BLOCK = _S3.start_block
S3_END_BLOCK = _S3.end_block
S3_EXPECTED_BLOCKS = _S3.blocks
S3_CANONICAL_TRANSACTIONS = _S3.canonical_transactions
assert S3_CANONICAL_TRANSACTIONS is not None
S3_EXPECTED_TRANSACTIONS = _S3.paper_transactions
S3_EXPECTED_LONGEST_CHAIN_SUM = _S3.paper_longest_chain_sum
S3_EXPECTED_RATIO = _S3.paper_ratio


def iter_blocks(path: Path) -> Iterable[dict]:
    with path.open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            value = json.loads(line)
            if not isinstance(value, dict):
                raise ValueError(f"{path}:{line_number}: block record must be a JSON object")
            yield value


def load_blocks(path: Path) -> list[dict]:
    return list(iter_blocks(path))


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


def compute_metrics(blocks: Iterable[dict]) -> dict:
    block_count = 0
    tx_count = 0
    longest_chain_sum = 0
    max_chain = 0
    max_chain_key = None
    contract_transactions: Counter[str] = Counter()
    contract_accesses: Counter[str] = Counter()
    longest_chain_contract_contribution: Counter[str] = Counter()

    for block in blocks:
        block_count += 1
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
        "blocks": block_count,
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


def validate_shape(blocks: Iterable[dict], start_block: int, end_block: int) -> list[str]:
    errors: list[str] = []
    expected_block = start_block
    observed_blocks = 0
    for block in blocks:
        observed_blocks += 1
        number = int(block.get("block_number", -1))
        if number != expected_block:
            errors.append(
                f"block numbers are not the exact contiguous range {start_block}..{end_block}: "
                f"expected {expected_block}, got {number}"
            )
            # Continue from the observed number so one missing block does not emit thousands of
            # cascading range errors. The final block-count/range check remains authoritative.
            expected_block = number
        expected_block += 1

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

    expected_count = end_block - start_block + 1
    if observed_blocks != expected_count or expected_block != end_block + 1:
        errors.append(
            f"block numbers are not the exact contiguous range {start_block}..{end_block}: "
            f"observed {observed_blocks} block records"
        )
    return errors
