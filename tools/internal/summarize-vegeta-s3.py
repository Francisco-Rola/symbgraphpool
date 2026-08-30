#!/usr/bin/env python3
"""Summarize the seven-strategy Vegeta S3 smoke with matched direct-Serial normalization."""

from __future__ import annotations

import argparse
import csv
import json
import statistics
from pathlib import Path


def load_records(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def match_key(record: dict):
    metadata = record["metadata"]
    params = tuple(sorted(metadata.get("parameters", {}).items()))
    return (metadata["workload"], metadata["workers"], metadata["seed"], params)


def nanos(record: dict, path: tuple[str, ...]) -> int:
    value = record
    for key in path:
        value = value.get(key, {})
    return int(value or 0)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--csv-output", type=Path, required=True)
    args = parser.parse_args()
    records = load_records(args.records)
    serial = {
        match_key(record): nanos(record, ("pipeline_timing", "total_adaptive_block_nanos"))
        for record in records
        if record["metadata"]["mode"] == "serial"
    }
    rows = []
    for record in records:
        metadata = record["metadata"]
        wall = nanos(record, ("pipeline_timing", "total_adaptive_block_nanos"))
        reference = serial.get(match_key(record))
        if not reference:
            raise SystemExit(f"missing matched serial control for run {metadata.get('run_index')}")
        rows.append(
            {
                "block": metadata["parameters"].get("vegeta.measured_block", "?"),
                "sample": metadata["parameters"].get("vegeta.sample", "?"),
                "mode": metadata["mode"],
                "transactions": record.get("execution", {}).get("transactions", 0),
                "matched_serial_speedup": reference / wall if wall else 0.0,
                "wall_ms": wall / 1_000_000.0,
                "post_consensus_ms": nanos(record, ("consensus", "post_consensus_nanos")) / 1_000_000.0,
                "replayed_transactions": record.get("execution", {}).get("replayed_transactions", 0),
                "serial_equivalent": record.get("correctness", {}).get("serial_equivalent"),
            }
        )

    args.csv_output.parent.mkdir(parents=True, exist_ok=True)
    with args.csv_output.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)

    ordered_modes = [
        "serial",
        "aria-fb",
        "vegeta",
        "exact-access",
        "static",
        "probability-only",
        "cost-aware",
    ]
    lines = [
        "Vegeta S3 trace-port seven-strategy smoke",
        "",
        "Normalization: exact matched direct-Serial block wall for the same Ethereum block and parameters.",
        "Access corpus: stock-Geth SLOAD/SSTORE trace port; see validation-report.json for paper-metric comparison.",
        "",
        "block sample mode transactions speedup wall-ms post-ms replay correct",
    ]
    for block in sorted({row["block"] for row in rows}, key=int):
        block_rows = {row["mode"]: row for row in rows if row["block"] == block}
        for mode in ordered_modes:
            row = block_rows.get(mode)
            if row is None:
                continue
            lines.append(
                f"{row['block']:>8} {row['sample']:<6} {mode:<18} {int(row['transactions']):>5} "
                f"{row['matched_serial_speedup']:>7.2f}x {row['wall_ms']:>8.2f} "
                f"{row['post_consensus_ms']:>7.2f} {int(row['replayed_transactions']):>5} "
                f"{str(row['serial_equivalent']):>5}"
            )
    lines.extend(["", "Overall median matched-Serial speedup:"])
    for mode in ordered_modes:
        values = [row["matched_serial_speedup"] for row in rows if row["mode"] == mode]
        if values:
            lines.append(f"  {mode:<18} {statistics.median(values):.2f}x")
    if not all(row["serial_equivalent"] is True for row in rows):
        lines.append("FAIL: at least one strategy was not serial-equivalent")
        status = 1
    else:
        lines.append(f"PASS: {len(rows)} records; every strategy is serial-equivalent.")
        status = 0
    rendered = "\n".join(lines) + "\n"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
