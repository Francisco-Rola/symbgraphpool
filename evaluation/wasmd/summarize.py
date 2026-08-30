#!/usr/bin/env python3
"""Summarize the controlled Wasmd scheduler evaluation.

The publication-facing throughput metric uses one campaign-wide consensus window C:
C = max measured pre_consensus_nanos across Rust-ACG and Vegeta records.

Effective service time per block for every strategy:
  C + post_consensus_nanos

Rust-ACG and Vegeta use C for their pre-consensus work; Serial, Block-STM, and
AriaFB are idle with respect to execution during the same consensus interval and then
perform their post-consensus work. Raw wall time and post-only speedup are reported
separately so implementation cost and consensus-visible execution remain explicit.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any

STRATEGY_ORDER = [
    "cosmos-wasmd-direct-serial",
    "cosmos-wasmd-block-stm",
    "cosmos-wasmd-aria-fb",
    "cosmos-wasmd-vegeta",
    "cosmos-wasmd-symbgraph-rust",
]
LABELS = {
    "cosmos-wasmd-direct-serial": "Serial",
    "cosmos-wasmd-block-stm": "BlockSTM",
    "cosmos-wasmd-aria-fb": "AriaFB",
    "cosmos-wasmd-vegeta": "Vegeta",
    "cosmos-wasmd-symbgraph-rust": "Rust-ACG",
}
PRECONSENSUS = {"cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust"}

# Two-sided 95% Student-t critical values by degrees of freedom. For larger n,
# the normal approximation is sufficient for the reporting precision here.
T95 = {
    1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447,
    7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228, 11: 2.201, 12: 2.179,
    13: 2.160, 14: 2.145, 15: 2.131, 16: 2.120, 17: 2.110, 18: 2.101,
    19: 2.093, 20: 2.086, 21: 2.080, 22: 2.074, 23: 2.069, 24: 2.064,
    25: 2.060, 26: 2.056, 27: 2.052, 28: 2.048, 29: 2.045, 30: 2.042,
}


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for line_no, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{line_no}: invalid JSON: {exc}") from exc
    return rows


def percentile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    xs = sorted(values)
    if len(xs) == 1:
        return xs[0]
    pos = (len(xs) - 1) * q
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return xs[lo]
    frac = pos - lo
    return xs[lo] * (1.0 - frac) + xs[hi] * frac


def mean_ci95(values: list[float]) -> tuple[float, float]:
    if not values:
        return 0.0, 0.0
    mean = statistics.fmean(values)
    if len(values) < 2:
        return mean, 0.0
    sd = statistics.stdev(values)
    critical = T95.get(len(values) - 1, 1.96)
    return mean, critical * sd / math.sqrt(len(values))


def validate_campaign_completeness(rows: list[dict[str, Any]]) -> None:
    """Require an identical block/transaction campaign for every strategy.

    Serial is the canonical key set for each (workers, sample). Publication
    summaries must never silently compare partial strategy output against a
    complete serial run.
    """
    keyed: dict[tuple[str, int, int], dict[int, int]] = defaultdict(dict)
    samples: set[tuple[int, int]] = set()
    for row in rows:
        strategy = row.get("strategy")
        if strategy not in STRATEGY_ORDER:
            continue
        workers = int(row["workers"])
        sample = int(row["sample"])
        block = int(row["block_number"])
        txs = int(row.get("transactions", 0))
        key = (strategy, workers, sample)
        if block in keyed[key]:
            raise SystemExit(
                f"duplicate Wasmd record strategy={strategy} workers={workers} "
                f"sample={sample} block={block}"
            )
        keyed[key][block] = txs
        samples.add((workers, sample))

    for workers, sample in sorted(samples):
        serial_key = ("cosmos-wasmd-direct-serial", workers, sample)
        serial = keyed.get(serial_key)
        if not serial:
            raise SystemExit(f"missing serial campaign workers={workers} sample={sample}")
        for strategy in STRATEGY_ORDER:
            actual = keyed.get((strategy, workers, sample))
            if actual is None:
                raise SystemExit(
                    f"missing strategy campaign strategy={strategy} workers={workers} sample={sample}"
                )
            if actual != serial:
                missing_blocks = sorted(set(serial) - set(actual))
                extra_blocks = sorted(set(actual) - set(serial))
                mismatched_txs = sorted(
                    b for b in set(serial) & set(actual) if serial[b] != actual[b]
                )
                raise SystemExit(
                    "incomplete/mismatched Wasmd campaign "
                    f"strategy={strategy} workers={workers} sample={sample} "
                    f"missing_blocks={missing_blocks[:8]} extra_blocks={extra_blocks[:8]} "
                    f"tx_count_mismatch_blocks={mismatched_txs[:8]}"
                )


def aggregate_samples(per_sample: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, int], list[dict[str, Any]]] = defaultdict(list)
    for row in per_sample:
        grouped[(row["strategy"], row["workers"])].append(row)

    metrics = [
        "throughput_tps", "throughput_speedup", "post_x", "wall_x", "wall_tps",
        "post_ms", "post_p50_ms", "post_p95_ms", "post_p99_ms", "wall_ms", "pre_p95_ms", "pre_max_ms", "consensus_headroom_p95_ms",
        "consensus_window_utilization_p95_pct", "replay_pct",
        "validation_ms", "replay_execution_ms", "conflict_analysis_ms",
    ]
    out: list[dict[str, Any]] = []
    for (strategy, workers), samples in grouped.items():
        samples = sorted(samples, key=lambda r: r["sample"])
        row: dict[str, Any] = {
            "strategy": strategy,
            "label": LABELS.get(strategy, strategy),
            "workers": workers,
            "samples": len(samples),
            "consensus_window_ms": samples[0]["consensus_window_ms"],
            "transactions": samples[0]["transactions"],
            "blocks": samples[0]["blocks"],
            "serial_equivalent": all(s["serial_equivalent"] for s in samples),
        }
        for metric in metrics:
            vals = [float(s[metric]) for s in samples]
            mean, ci = mean_ci95(vals)
            row[metric] = mean
            row[f"{metric}_ci95"] = ci
            row[f"{metric}_median"] = statistics.median(vals)
        row["reexecutions"] = statistics.fmean(float(s["reexecutions"]) for s in samples)
        row["forward_fallbacks"] = statistics.fmean(float(s["forward_fallbacks"]) for s in samples)
        row["safety_replays"] = statistics.fmean(float(s["safety_replays"]) for s in samples)
        out.append(row)
    order = {s: i for i, s in enumerate(STRATEGY_ORDER)}
    out.sort(key=lambda r: (r["workers"], order.get(r["strategy"], 999), r["strategy"]))
    return out


def build_per_sample(rows: list[dict[str, Any]], window_ns: int) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, int, int], list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        strategy = row.get("strategy")
        if strategy not in STRATEGY_ORDER:
            continue
        grouped[(strategy, int(row["workers"]), int(row["sample"]))].append(row)

    out: list[dict[str, Any]] = []
    for (strategy, workers, sample), rs in grouped.items():
        rs.sort(key=lambda r: int(r["block_number"]))
        blocks = len(rs)
        txs = sum(int(r.get("transactions", 0)) for r in rs)
        serial_ns = sum(int(r.get("matched_serial_nanos", 0)) for r in rs)
        wall_ns = sum(int(r.get("strategy_total_nanos", 0)) for r in rs)
        post_values = [int(r.get("post_consensus_nanos", 0) or r.get("strategy_total_nanos", 0)) for r in rs]
        post_ns = sum(post_values)
        pre_values = [int(r.get("pre_consensus_nanos", 0)) for r in rs]
        effective_ns = post_ns + window_ns * blocks
        validation_ns = sum(int(r.get("validation_nanos", 0)) for r in rs)
        replay_exec_ns = sum(int(r.get("replay_execution_nanos", 0)) for r in rs)
        conflict_ns = sum(int(r.get("conflict_analysis_nanos", 0)) for r in rs)
        reexec = sum(int(r.get("reexecutions", 0)) for r in rs)
        forward = sum(int(r.get("forward_fallbacks", 0)) for r in rs)
        safety = sum(int(r.get("safety_replays", 0)) for r in rs)
        serial_equivalent = all(bool(r.get("serial_equivalent", False)) for r in rs)
        if not serial_equivalent:
            raise SystemExit(f"state-equivalence failure strategy={strategy} workers={workers} sample={sample}")
        out.append({
            "strategy": strategy,
            "label": LABELS.get(strategy, strategy),
            "workers": workers,
            "sample": sample,
            "blocks": blocks,
            "transactions": txs,
            "consensus_window_ms": window_ns / 1e6,
            "effective_service_ms": effective_ns / 1e6,
            "throughput_tps": (txs * 1e9 / effective_ns) if effective_ns else 0.0,
            "post_ms": post_ns / 1e6,
            "post_p50_ms": percentile([x / 1e6 for x in post_values], 0.50),
            "post_p95_ms": percentile([x / 1e6 for x in post_values], 0.95),
            "post_p99_ms": percentile([x / 1e6 for x in post_values], 0.99),
            "post_x": (serial_ns / post_ns) if post_ns else 0.0,
            "wall_ms": wall_ns / 1e6,
            "wall_x": (serial_ns / wall_ns) if wall_ns else 0.0,
            "wall_tps": (txs * 1e9 / wall_ns) if wall_ns else 0.0,
            "pre_p50_ms": percentile([x / 1e6 for x in pre_values], 0.50),
            "pre_p95_ms": percentile([x / 1e6 for x in pre_values], 0.95),
            "pre_p99_ms": percentile([x / 1e6 for x in pre_values], 0.99),
            "pre_max_ms": max(pre_values, default=0) / 1e6,
            "consensus_headroom_p95_ms": (
                (window_ns / 1e6) - percentile([x / 1e6 for x in pre_values], 0.95)
                if strategy in PRECONSENSUS else window_ns / 1e6
            ),
            "consensus_window_utilization_p95_pct": (
                100.0 * percentile(pre_values, 0.95) / window_ns
                if strategy in PRECONSENSUS and window_ns else 0.0
            ),
            "validation_ms": validation_ns / 1e6,
            "replay_execution_ms": replay_exec_ns / 1e6,
            "conflict_analysis_ms": conflict_ns / 1e6,
            "reexecutions": reexec,
            "replay_pct": (100.0 * reexec / txs) if txs else 0.0,
            "forward_fallbacks": forward,
            "safety_replays": safety,
            "serial_equivalent": serial_equivalent,
        })

    serial_by_sample = {
        (r["workers"], r["sample"]): r["throughput_tps"]
        for r in out if r["strategy"] == "cosmos-wasmd-direct-serial"
    }
    for row in out:
        serial_tps = serial_by_sample.get((row["workers"], row["sample"]), 0.0)
        row["throughput_speedup"] = row["throughput_tps"] / serial_tps if serial_tps else 0.0
    order = {s: i for i, s in enumerate(STRATEGY_ORDER)}
    out.sort(key=lambda r: (r["workers"], r["sample"], order.get(r["strategy"], 999)))
    return out


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    if not rows:
        path.write_text("", encoding="utf-8")
        return
    cols: list[str] = []
    for row in rows:
        for key in row:
            if key not in cols:
                cols.append(key)
    with path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=cols)
        w.writeheader()
        w.writerows(rows)


def render(rows: list[dict[str, Any]], window_ns: int) -> str:
    lines = [
        "Wasmd controlled scheduler evaluation",
        "",
        f"Campaign consensus window: {window_ns / 1e6:.3f} ms",
        "  C = maximum measured pre-consensus interval across Rust-ACG and Vegeta in this campaign.",
        "  Throughput denominator: every strategy uses C + post; ACG/Vegeta perform pre-execution inside C.",
        "  post-x = matched serial execution / consensus-visible post phase. wall-x is bookkeeping only.",
        "",
        f"{'system':<12} {'w':>3} {'n':>3} {'tps':>10} {'tput-x':>7} {'post-ms':>10} {'post-x':>7} {'wall-x':>7} {'replay':>8} {'val-ms':>9} {'replay-ms':>10}",
    ]
    for r in rows:
        lines.append(
            f"{r['label']:<12} {r['workers']:>3} {r['samples']:>3} "
            f"{r['throughput_tps']:>10.1f} {r['throughput_speedup']:>7.2f} "
            f"{r['post_ms']:>10.1f} {r['post_x']:>7.2f} {r['wall_x']:>7.2f} "
            f"{r['replay_pct']:>7.2f}% {r['validation_ms']:>9.1f} {r['replay_execution_ms']:>10.1f}"
        )
    lines += [
        "",
        "Reporting notes:",
        "  * Use throughput_tps as the primary fixed-consensus-window throughput metric.",
        "  * Report post-x beside throughput: it isolates consensus-visible validation/replay from serial execution.",
        "  * Report wall-x and pre-consensus percentiles to show the real resource cost and whether speculation fits C.",
        "  * AriaFB ports the attached repository's exact Rule-2 abort condition and hot-chain DAG fallback to Wasmd.",
        "  * Vegeta ports SpeculateMod/ParallelMod hot-key proposal reordering, Rule-2 replay batches, and access-change handling to Wasmd.",
        "  * serial_equivalent and matched_serial_nanos use each strategy's serial_reference_scope; historical_serial_nanos retains the common historical-order control.",
        "  * safety_replays are conservative Wasmd-only fallbacks for dynamic key/range changes absent from the Ethereum access model.",
    ]
    return "\n".join(lines) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--records", required=True, type=Path)
    ap.add_argument("--output-dir", required=True, type=Path)
    args = ap.parse_args()

    rows = read_jsonl(args.records)
    required = set(STRATEGY_ORDER)
    present = {r.get("strategy") for r in rows}
    missing = sorted(required - present)
    if missing:
        raise SystemExit(f"missing Wasmd strategy rows: {', '.join(missing)}")
    if not all(bool(r.get("serial_equivalent", False)) for r in rows if r.get("strategy") in required):
        raise SystemExit("one or more Wasmd records failed serial state equivalence")
    validate_campaign_completeness(rows)

    pre = [
        int(r.get("pre_consensus_nanos", 0))
        for r in rows if r.get("strategy") in PRECONSENSUS
    ]
    window_ns = max(pre, default=0)
    if window_ns <= 0:
        raise SystemExit("cannot derive campaign consensus window: no positive Rust-ACG/Vegeta pre-consensus measurement")

    per_sample = build_per_sample(rows, window_ns)
    aggregated = aggregate_samples(per_sample)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    write_csv(args.output_dir / "per-sample.csv", per_sample)
    write_csv(args.output_dir / "summary.csv", aggregated)
    obj = {
        "schema_version": 1,
        "consensus_window_nanos": window_ns,
        "consensus_window_definition": "max pre_consensus_nanos across Rust-ACG and Vegeta for the entire campaign",
        "throughput_definition": {
            "all_strategies": "transactions / (blocks * consensus_window + sum(post_consensus))",
            "interpretation": "Rust-ACG/Vegeta use the consensus interval for pre-execution; Serial/BlockSTM/AriaFB wait for the same fixed consensus interval before post-consensus execution.",
        },
        "rows": aggregated,
        "per_sample": per_sample,
    }
    (args.output_dir / "summary.json").write_text(json.dumps(obj, indent=2) + "\n", encoding="utf-8")
    text = render(aggregated, window_ns)
    (args.output_dir / "summary.txt").write_text(text, encoding="utf-8")
    print(text, end="")


if __name__ == "__main__":
    main()
