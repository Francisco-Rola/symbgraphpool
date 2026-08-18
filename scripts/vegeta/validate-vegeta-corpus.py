#!/usr/bin/env python3
"""Validate a reconstructed Vegeta Ethereum corpus against the paper's published S3 aggregates."""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
from vegeta_corpus import (  # noqa: E402
    S3_END_BLOCK,
    S3_CANONICAL_TRANSACTIONS,
    S3_EXPECTED_BLOCKS,
    S3_EXPECTED_LONGEST_CHAIN_SUM,
    S3_EXPECTED_RATIO,
    S3_EXPECTED_TRANSACTIONS,
    S3_START_BLOCK,
    WETH_MAINNET,
    compute_metrics,
    load_blocks,
    validate_shape,
)


def validate_metrics(
    metrics: dict,
    require_paper_chain_match: bool = False,
    require_weth_hotspot: bool = False,
) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    warnings: list[str] = []

    if metrics["blocks"] != S3_EXPECTED_BLOCKS:
        errors.append(f"expected {S3_EXPECTED_BLOCKS} blocks, got {metrics['blocks']}")
    if metrics["transactions"] != S3_CANONICAL_TRANSACTIONS:
        errors.append(
            "canonical Ethereum S3 range transaction count mismatch: "
            f"expected {S3_CANONICAL_TRANSACTIONS}, got {metrics['transactions']}"
        )
    if S3_CANONICAL_TRANSACTIONS != S3_EXPECTED_TRANSACTIONS:
        warnings.append(
            "Vegeta paper metadata reports "
            f"{S3_EXPECTED_TRANSACTIONS} transactions for blocks "
            f"{S3_START_BLOCK}..{S3_END_BLOCK}; canonical Ethereum reconstruction "
            f"of that exact range contains {S3_CANONICAL_TRANSACTIONS}. "
            "The canonical count is used for corpus identity; the paper count is retained "
            "for provenance only."
        )

    if require_paper_chain_match:
        if metrics["longest_chain_sum"] != S3_EXPECTED_LONGEST_CHAIN_SUM:
            errors.append(
                "storage-trace longest-chain sum does not match Vegeta paper: "
                f"expected {S3_EXPECTED_LONGEST_CHAIN_SUM}, got {metrics['longest_chain_sum']}"
            )
        if metrics["ratio"] is None or not math.isclose(
            metrics["ratio"], S3_EXPECTED_RATIO, rel_tol=0.0, abs_tol=0.01
        ):
            errors.append(
                f"expected ratio {S3_EXPECTED_RATIO:.2f}, got {metrics['ratio']}"
            )
    if (
        require_weth_hotspot
        and metrics["dominant_longest_chain_contract"] != WETH_MAINNET
    ):
        errors.append(
            "WETH does not dominate the summed per-block longest-chain contribution in this "
            "reconstructed corpus: "
            f"got {metrics['dominant_longest_chain_contract']}"
        )

    return errors, warnings


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    parser.add_argument(
        "--require-paper-chain-match",
        action="store_true",
        help="also require the storage-only tracer to reproduce Vegeta's unpublished chain metric exactly",
    )
    parser.add_argument(
        "--require-weth-hotspot",
        action="store_true",
        help="require WETH to dominate the summed per-block longest-chain contribution",
    )
    parser.add_argument("--json-output", type=Path)
    args = parser.parse_args()

    blocks = load_blocks(args.corpus)
    errors = validate_shape(blocks, S3_START_BLOCK, S3_END_BLOCK)
    metrics = compute_metrics(blocks)
    metric_errors, warnings = validate_metrics(
        metrics,
        require_paper_chain_match=args.require_paper_chain_match,
        require_weth_hotspot=args.require_weth_hotspot,
    )
    errors.extend(metric_errors)

    report = {
        "canonical_reconstruction": {
            "block_range": [S3_START_BLOCK, S3_END_BLOCK],
            "blocks": S3_EXPECTED_BLOCKS,
            "transactions": S3_CANONICAL_TRANSACTIONS,
        },
        "paper": {
            "blocks": S3_EXPECTED_BLOCKS,
            "transactions": S3_EXPECTED_TRANSACTIONS,
            "longest_chain_sum": S3_EXPECTED_LONGEST_CHAIN_SUM,
            "ratio": S3_EXPECTED_RATIO,
            "weth_hotspot_reported": True,
        },
        "observed": metrics,
        "instrumentation_note": (
            "Block identity and transaction count are validated against the canonical Ethereum "
            "mainnet reconstruction of the stated S3 range. Vegeta does not publish its exact "
            "internal read/write corpus, so longest-chain/WETH checks remain optional "
            "instrumentation-equivalence diagnostics."
        ),
        "warnings": warnings,
        "errors": errors,
        "accepted": not errors,
    }
    rendered = json.dumps(report, indent=2, sort_keys=True)
    print(rendered)
    if args.json_output:
        args.json_output.parent.mkdir(parents=True, exist_ok=True)
        args.json_output.write_text(rendered + "\n", encoding="utf-8")
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
