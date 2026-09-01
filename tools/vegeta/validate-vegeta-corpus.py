#!/usr/bin/env python3
"""Validate a reconstructed Vegeta Ethereum corpus against a published dataset range."""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
from vegeta_corpus import (  # noqa: E402
    VEGETA_DATASETS,
    WETH_MAINNET,
    VegetaDatasetSpec,
    compute_metrics,
    dataset_by_tag,
    iter_blocks,
    validate_shape,
)


def validate_metrics(
    metrics: dict,
    dataset: VegetaDatasetSpec | None = None,
    require_paper_chain_match: bool = False,
    require_weth_hotspot: bool = False,
    require_paper_tx_count: bool = False,
) -> tuple[list[str], list[str]]:
    """Validate aggregate metrics.

    ``dataset`` defaults to S3 for backward compatibility with the original S3-only validator.
    A transaction count becomes a hard corpus-identity check only after this repository has frozen
    an independent canonical reconstruction for that range. Otherwise the paper count remains a
    provenance target unless ``require_paper_tx_count`` is explicitly requested.
    """

    spec = dataset or dataset_by_tag("S3")
    errors: list[str] = []
    warnings: list[str] = []

    if metrics["blocks"] != spec.blocks:
        errors.append(f"expected {spec.blocks} blocks, got {metrics['blocks']}")

    if spec.canonical_transactions is not None:
        if metrics["transactions"] != spec.canonical_transactions:
            errors.append(
                f"canonical Ethereum {spec.tag} range transaction count mismatch: "
                f"expected {spec.canonical_transactions}, got {metrics['transactions']}"
            )
        if spec.canonical_transactions != spec.paper_transactions:
            warnings.append(
                "Vegeta paper metadata reports "
                f"{spec.paper_transactions} transactions for blocks "
                f"{spec.start_block}..{spec.end_block}; canonical Ethereum reconstruction "
                f"of that exact range contains {spec.canonical_transactions}. "
                "The canonical count is used for corpus identity; the paper count is retained "
                "for provenance only."
            )
    elif metrics["transactions"] != spec.paper_transactions:
        message = (
            f"Vegeta paper reports {spec.paper_transactions} transactions for {spec.tag} blocks "
            f"{spec.start_block}..{spec.end_block}, while this canonical RPC reconstruction "
            f"contains {metrics['transactions']}. No independent canonical transaction-count "
            "freeze exists for this range yet."
        )
        if require_paper_tx_count:
            errors.append(message)
        else:
            warnings.append(message)

    if require_paper_chain_match:
        if metrics["longest_chain_sum"] != spec.paper_longest_chain_sum:
            errors.append(
                "storage-trace longest-chain sum does not match Vegeta paper: "
                f"expected {spec.paper_longest_chain_sum}, got {metrics['longest_chain_sum']}"
            )
        if metrics["ratio"] is None or not math.isclose(
            metrics["ratio"], spec.paper_ratio, rel_tol=0.0, abs_tol=0.01
        ):
            errors.append(
                f"expected ratio {spec.paper_ratio:.2f}, got {metrics['ratio']}"
            )
    if require_weth_hotspot and metrics["dominant_longest_chain_contract"] != WETH_MAINNET:
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
        "--dataset-tag",
        choices=sorted(VEGETA_DATASETS),
        default="S3",
        help="Vegeta NSDI'25 Table-2 dataset to validate (default: S3)",
    )
    parser.add_argument(
        "--require-paper-tx-count",
        action="store_true",
        help=(
            "for ranges without an independently frozen canonical count, require the reconstructed "
            "transaction count to equal the paper metadata"
        ),
    )
    parser.add_argument(
        "--require-paper-chain-match",
        action="store_true",
        help="also require the storage tracer to reproduce Vegeta's unpublished chain metric exactly",
    )
    parser.add_argument(
        "--require-weth-hotspot",
        action="store_true",
        help="require WETH to dominate the summed per-block longest-chain contribution",
    )
    parser.add_argument("--json-output", type=Path)
    args = parser.parse_args()

    spec = dataset_by_tag(args.dataset_tag)
    errors = validate_shape(iter_blocks(args.corpus), spec.start_block, spec.end_block)
    metrics = compute_metrics(iter_blocks(args.corpus))
    metric_errors, warnings = validate_metrics(
        metrics,
        dataset=spec,
        require_paper_chain_match=args.require_paper_chain_match,
        require_weth_hotspot=args.require_weth_hotspot,
        require_paper_tx_count=args.require_paper_tx_count,
    )
    errors.extend(metric_errors)

    report = {
        "dataset": spec.tag,
        "canonical_reconstruction": {
            "block_range": [spec.start_block, spec.end_block],
            "blocks": spec.blocks,
            "transactions": spec.canonical_transactions,
        },
        "paper": {
            "blocks": spec.blocks,
            "transactions": spec.paper_transactions,
            "longest_chain_sum": spec.paper_longest_chain_sum,
            "ratio": spec.paper_ratio,
            "weth_hotspot_reported": True,
        },
        "observed": metrics,
        "instrumentation_note": (
            "Block identity and transaction order are validated against the stated Ethereum "
            "mainnet range. Vegeta does not publish its exact internal read/write corpus, so "
            "longest-chain/WETH checks remain optional instrumentation-equivalence diagnostics. "
            "A paper transaction count is a hard identity check only when an independent canonical "
            "count has been frozen for that range or --require-paper-tx-count is supplied."
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
