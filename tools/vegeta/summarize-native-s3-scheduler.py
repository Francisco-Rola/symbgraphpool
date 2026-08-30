#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Iterable

STRATEGIES = (
    "serial",
    "aria-fb",
    "vegeta",
    "exact-access",
    "static",
    "probability-only",
    "cost-aware",
)


def read_records(path: Path) -> list[dict]:
    rows = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"{path}:{lineno}: invalid JSON: {exc}") from exc
        rows.append(row)
    if not rows:
        raise ValueError(f"no scheduler records in {path}")
    return rows


def percentile(values: Iterable[float], q: float) -> float:
    vals = sorted(values)
    if not vals:
        return 0.0
    if len(vals) == 1:
        return vals[0]
    pos = (len(vals) - 1) * q
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return vals[lo]
    weight = pos - lo
    return vals[lo] * (1.0 - weight) + vals[hi] * weight


def per_sample(rows: list[dict]) -> list[dict]:
    grouped: dict[tuple[int, str, int], list[dict]] = defaultdict(list)
    for row in rows:
        grouped[(int(row["workers"]), str(row["strategy"]), int(row["sample"]))].append(row)
    out = []
    for (workers, strategy, sample), group in sorted(grouped.items()):
        serial = sum(int(r["matched_serial_nanos"]) for r in group)
        total = sum(int(r["strategy_total_nanos"]) for r in group)
        post = sum(int(r["post_consensus_nanos"]) for r in group)
        txs = sum(int(r["transactions"]) for r in group)
        replay = sum(int(r["replayed_transactions"]) for r in group)
        canonical = sum(int(r["canonical_transactions"]) for r in group)
        reused = sum(int(r["reused_receipts"]) for r in group)
        prepared = sum(int(r["prepared_receipts"]) for r in group)
        out.append({
            "workers": workers,
            "strategy": strategy,
            "sample": sample,
            "blocks": len(group),
            "transactions": txs,
            "aggregate_active_wall_speedup": serial / total if total else 0.0,
            "aggregate_post_consensus_speedup": serial / post if post else 0.0,
            "median_block_speedup": statistics.median(float(r["matched_serial_speedup"]) for r in group),
            "median_active_wall_ms": statistics.median(int(r["strategy_total_nanos"]) for r in group) / 1e6,
            "median_consensus_bottleneck_ms": statistics.median(int(r.get("consensus_bottleneck_nanos", 0)) for r in group) / 1e6,
            "median_post_consensus_ms": statistics.median(int(r["post_consensus_nanos"]) for r in group) / 1e6,
            "p95_post_consensus_ms": percentile([int(r["post_consensus_nanos"]) / 1e6 for r in group], 0.95),
            "median_planning_ms": statistics.median(int(r["planning_nanos"]) for r in group) / 1e6,
            "median_preexecution_ms": statistics.median(int(r["preexecution_nanos"]) for r in group) / 1e6,
            "median_reconciliation_ms": statistics.median(int(r["reconciliation_nanos"]) for r in group) / 1e6,
            "median_feedback_ms": statistics.median(int(r.get("feedback_nanos", 0)) for r in group) / 1e6,
            "replay_rate": replay / txs if txs else 0.0,
            "canonical_missing_rate": canonical / txs if txs else 0.0,
            "receipt_reuse_rate": reused / prepared if prepared else 0.0,
            "serial_bypass_block_rate": sum(bool(r["serial_bypassed"]) for r in group) / len(group),
            "median_dependency_edges": statistics.median(int(r["dependency_edges"]) for r in group),
            "median_max_wave_width": statistics.median(int(r["max_wave_width"]) for r in group),
            "all_serial_equivalent": all(bool(r["serial_equivalent"]) for r in group),
        })
    return out


def summarize(rows: list[dict]) -> dict:
    sample_rows = per_sample(rows)
    grouped: dict[tuple[int, str], list[dict]] = defaultdict(list)
    for row in sample_rows:
        grouped[(row["workers"], row["strategy"])].append(row)
    strategy_rows = []
    metric_names = [
        "aggregate_active_wall_speedup",
        "aggregate_post_consensus_speedup",
        "median_block_speedup",
        "median_active_wall_ms",
        "median_consensus_bottleneck_ms",
        "median_post_consensus_ms",
        "p95_post_consensus_ms",
        "median_planning_ms",
        "median_preexecution_ms",
        "median_reconciliation_ms",
        "median_feedback_ms",
        "replay_rate",
        "canonical_missing_rate",
        "receipt_reuse_rate",
        "serial_bypass_block_rate",
        "median_dependency_edges",
        "median_max_wave_width",
    ]
    for (workers, strategy), group in sorted(grouped.items()):
        row = {
            "workers": workers,
            "strategy": strategy,
            "samples": len(group),
            "blocks_per_sample": min(g["blocks"] for g in group),
            "transactions_per_sample": min(g["transactions"] for g in group),
            "all_serial_equivalent": all(g["all_serial_equivalent"] for g in group),
        }
        for name in metric_names:
            values = [float(g[name]) for g in group]
            row[name] = statistics.median(values)
            row[f"{name}_min"] = min(values)
            row[f"{name}_max"] = max(values)
        strategy_rows.append(row)
    return {
        "schema_version": 1,
        "dataset": "vegeta-s3-native-scheduler-summary",
        "record_count": len(rows),
        "strategies": strategy_rows,
        "per_sample": sample_rows,
        "interpretation_policy": "report-only; no performance outcome threshold",
    }


def render(report: dict) -> str:
    lines = [
        "Vegeta S3 native scheduler performance summary",
        "",
        "Medians are across independent full-range samples; each sample aggregates all 101 blocks.",
        "Active-wall speedup counts planning/preexecution/reconciliation/feedback compute.",
        "Post-consensus speedup compares matched serial wall with consensus-visible tail only.",
        "",
        "strategy              active-x   post-x   post-med-ms  post-p95-ms  replay%  reuse%  bypass%  serial-eq",
    ]
    for row in sorted(report["strategies"], key=lambda r: (r["workers"], STRATEGIES.index(r["strategy"]))):
        lines.append(
            f"{row['strategy']:<20} "
            f"{row['aggregate_active_wall_speedup']:8.3f} "
            f"{row['aggregate_post_consensus_speedup']:8.3f} "
            f"{row['median_post_consensus_ms']:12.3f} "
            f"{row['p95_post_consensus_ms']:12.3f} "
            f"{row['replay_rate']*100:7.2f} "
            f"{row['receipt_reuse_rate']*100:7.2f} "
            f"{row['serial_bypass_block_rate']*100:8.2f} "
            f"{'yes' if row['all_serial_equivalent'] else 'NO'}"
        )
    lines.extend([
        "",
        "No speedup/latency/replay threshold is applied by this summarizer.",
    ])
    return "\n".join(lines) + "\n"


def write_csv(path: Path, rows: list[dict]) -> None:
    if not rows:
        return
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0].keys()))
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--records", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    rows = read_records(args.records)
    report = summarize(rows)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "summary.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (args.output_dir / "summary.txt").write_text(render(report), encoding="utf-8")
    write_csv(args.output_dir / "summary.csv", report["strategies"])
    write_csv(args.output_dir / "per-sample.csv", report["per_sample"])
    print(render(report), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
