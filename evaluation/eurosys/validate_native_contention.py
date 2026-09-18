#!/usr/bin/env python3
"""Fail closed when MiniWarehouse is confounded by canonical fallback.

The native contention sweep is intended to vary application conflicts, not whether a
baseline can execute the contract semantics from a speculative snapshot.  AriaFB and
Vegeta therefore must complete MiniWarehouse without the evaluator's whole-block
historical-order safety fallback.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

BASELINES = {
    "cosmos-wasmd-aria-fb": (
        "aria_canonical_fallback",
        "aria_historical_fallback_transactions",
        "aria_historical_fallback_nanos",
    ),
    "cosmos-wasmd-vegeta": (
        "vegeta_canonical_fallback",
        "vegeta_historical_fallback_transactions",
        "vegeta_historical_fallback_nanos",
    ),
}


def load(path: Path) -> list[dict]:
    rows = []
    with path.open(encoding="utf-8") as handle:
        for line_no, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{line_no}: invalid JSON: {exc}") from exc
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("records", type=Path)
    args = parser.parse_args()
    if not args.records.is_file():
        raise SystemExit(f"missing MiniWarehouse records: {args.records}")

    rows = load(args.records)
    failures = []
    for strategy, (flag_key, tx_key, nanos_key) in BASELINES.items():
        strategy_rows = [row for row in rows if row.get("strategy") == strategy]
        if not strategy_rows:
            failures.append(f"missing {strategy} records")
            continue
        fallback_rows = [
            row
            for row in strategy_rows
            if bool(row.get(flag_key, False))
            or int(row.get(tx_key, 0) or 0) > 0
            or int(row.get(nanos_key, 0) or 0) > 0
        ]
        if fallback_rows:
            first = fallback_rows[0]
            failures.append(
                f"{strategy}: canonical fallback on {len(fallback_rows)}/{len(strategy_rows)} rows "
                f"(first sample={first.get('sample')} block={first.get('block_number')}, "
                f"transactions={first.get(tx_key, 0)}, nanos={first.get(nanos_key, 0)})"
            )
        missing_post = [row for row in strategy_rows if "post_consensus_nanos" not in row]
        if missing_post:
            first = missing_post[0]
            failures.append(
                f"{strategy}: post_consensus_nanos missing on {len(missing_post)} rows "
                f"(first sample={first.get('sample')} block={first.get('block_number')})"
            )

    if failures:
        raise SystemExit(
            "MiniWarehouse contention validity gate failed:\n  - " + "\n  - ".join(failures)
        )
    print("PASS: MiniWarehouse AriaFB/Vegeta run without canonical fallback and expose post-order latency")


if __name__ == "__main__":
    main()
