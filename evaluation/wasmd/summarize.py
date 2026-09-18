#!/usr/bin/env python3
"""Summarize the controlled Wasmd scheduler evaluation.

Replay throughput follows Vegeta NSDI'25 Figure 10 and is retained for direct
prior-work comparison. The architectural view additionally sweeps an externally
supplied consensus window C and reports both the remaining execution tail
R+max(0,P-C) and proposal-to-commit time max(C,P)+R. No C value is treated as
measured consensus latency in this single-node harness.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "vegeta"))
from vegeta_corpus import dataset_by_tag  # noqa: E402

STRATEGY_ORDER = [
    "cosmos-wasmd-direct-serial",
    "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
    "cosmos-wasmd-block-stm",
    "cosmos-wasmd-aria-fb",
    "cosmos-wasmd-vegeta",
    "cosmos-wasmd-symbgraph-rust",
]
LABELS = {
    "cosmos-wasmd-direct-serial": "Serial",
    "cosmos-wasmd-symbgraph-rust-exact-trace-oracle": "ACG-Oracle",
    "cosmos-wasmd-block-stm": "BlockSTM",
    "cosmos-wasmd-aria-fb": "AriaFB",
    "cosmos-wasmd-vegeta": "Vegeta",
    "cosmos-wasmd-symbgraph-rust": "Rust-ACG",
}
PRECONSENSUS = {"cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust", "cosmos-wasmd-symbgraph-rust-exact-trace-oracle"}
CONSENSUS_WINDOW_STRATEGIES = {"cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust"}
DEFAULT_CONSENSUS_WINDOW_MS = 300.0
DEFAULT_CONSENSUS_WINDOWS_MS = (DEFAULT_CONSENSUS_WINDOW_MS,)


def record_post_consensus_nanos(row: dict[str, Any]) -> int:
    """Return consensus-visible post-order work without treating zero as missing.

    Older raw records kept harness-required Aria/Vegeta canonical restoration in a
    dedicated diagnostic but excluded it from ``post_consensus_nanos``.  New records
    mark that the fallback has already been charged; this compatibility path repairs
    old records exactly once so postprocessing remains reproducible.
    """
    if "post_consensus_nanos" in row:
        post = int(row.get("post_consensus_nanos", 0) or 0)
    elif row.get("strategy") in PRECONSENSUS:
        # The Go encoder historically omitted an explicit zero.  For pre-consensus
        # strategies that means zero post work, not "fall back to total wall time".
        post = 0
    else:
        post = int(row.get("strategy_total_nanos", 0) or 0)

    if not bool(row.get("post_consensus_includes_canonical_fallback", False)):
        post += int(row.get("aria_historical_fallback_nanos", 0) or 0)
        post += int(row.get("vegeta_historical_fallback_nanos", 0) or 0)
    return post

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


def _source_tx_cost(tx: dict[str, Any]) -> int:
    # S1/S4 public corpora retain gas_used as the cost proxy used by the Wasmd
    # campaign. Fall back to one so metadata-only transactions remain visible
    # in an unweighted critical path instead of disappearing from the bound.
    for key in ("gas_used", "source_compute_proxy", "gas_used_compute_proxy"):
        try:
            value = int(tx.get(key, 0) or 0)
        except (TypeError, ValueError):
            value = 0
        if value > 0:
            return value
    return 1


def source_prefix_parallelism(path: Path, block_numbers: set[int], workers: int) -> dict[str, Any]:
    """Measure source-Ethereum prefix structure without exact SLOAD/SSTORE traces.

    The hot-key ratio is the same touched-key definition used by the repository's
    Vegeta corpus validator.  The conflict DAG uses canonical historical order:
    RAW/WAW depend on the latest writer and WAR depends on readers since the last
    write.  Gas weights provide a cost-sensitive critical-path bound.  This is a
    source-workload diagnostic, not a scheduler oracle.
    """
    found: set[int] = set()
    blocks = 0
    transactions = 0
    longest_chain_sum = 0
    weighted_longest_chain_cost_sum = 0
    hot_key_worker_lower_bound_sum = 0
    conflict_cp_tx_sum = 0
    total_cost_sum = 0
    conflict_cp_cost_sum = 0
    worker_lower_bound_sum = 0

    with path.open(encoding="utf-8") as handle:
        for lineno, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                block = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{lineno}: invalid source corpus JSON: {exc}") from exc
            bn = int(block.get("block_number", -1))
            if bn not in block_numbers:
                continue
            if bn in found:
                raise SystemExit(f"duplicate source corpus block {bn}: {path}")
            found.add(bn)
            blocks += 1
            txs = list(block.get("transactions", []))
            transactions += len(txs)

            per_key: dict[str, int] = defaultdict(int)
            per_key_cost: dict[str, int] = defaultdict(int)
            last_writer: dict[str, int] = {}
            readers: dict[str, set[int]] = defaultdict(set)
            cp_tx: list[int] = []
            cp_cost: list[int] = []
            block_total_cost = 0
            block_max_cp_tx = 0
            block_max_cp_cost = 0

            for idx, tx in enumerate(txs):
                reads = set(tx.get("reads", ()) or ())
                writes = set(tx.get("writes", ()) or ())
                weight = _source_tx_cost(tx)
                for key in reads | writes:
                    per_key[key] += 1
                    per_key_cost[key] += weight

                preds: set[int] = set()
                for key in reads:
                    writer = last_writer.get(key)
                    if writer is not None:
                        preds.add(writer)
                    readers[key].add(idx)
                for key in writes:
                    writer = last_writer.get(key)
                    if writer is not None:
                        preds.add(writer)
                    preds.update(r for r in readers.get(key, ()) if r != idx)
                    readers[key].clear()
                    last_writer[key] = idx

                block_total_cost += weight
                tx_cp = 1 + max((cp_tx[pred] for pred in preds), default=0)
                cost_cp = weight + max((cp_cost[pred] for pred in preds), default=0)
                cp_tx.append(tx_cp)
                cp_cost.append(cost_cp)
                block_max_cp_tx = max(block_max_cp_tx, tx_cp)
                block_max_cp_cost = max(block_max_cp_cost, cost_cp)

            weighted_longest = 0
            if per_key:
                longest_chain_sum += max(per_key.values())
                weighted_longest = max(per_key_cost.values())
                weighted_longest_chain_cost_sum += weighted_longest
            conflict_cp_tx_sum += block_max_cp_tx
            total_cost_sum += block_total_cost
            conflict_cp_cost_sum += block_max_cp_cost
            worker_capacity = math.ceil(block_total_cost / max(1, workers)) if block_total_cost else 0
            hot_key_worker_lower_bound_sum += max(weighted_longest, worker_capacity)
            worker_lower_bound_sum += max(block_max_cp_cost, worker_capacity)
            if len(found) == len(block_numbers):
                break

    missing = sorted(block_numbers - found)
    if missing:
        raise SystemExit(f"source corpus is missing benchmark blocks: {missing[:8]}")
    return {
        "blocks": blocks,
        "transactions": transactions,
        "hot_key_chain_ratio": (transactions / longest_chain_sum) if longest_chain_sum else 0.0,
        "weighted_hot_key_parallelism": (total_cost_sum / weighted_longest_chain_cost_sum) if weighted_longest_chain_cost_sum else 0.0,
        "hot_key_ideal_worker_speedup": (total_cost_sum / hot_key_worker_lower_bound_sum) if hot_key_worker_lower_bound_sum else 0.0,
        "conflict_dag_parallelism": (transactions / conflict_cp_tx_sum) if conflict_cp_tx_sum else 0.0,
        "weighted_dag_parallelism": (total_cost_sum / conflict_cp_cost_sum) if conflict_cp_cost_sum else 0.0,
        "conflict_dag_ideal_worker_speedup": (total_cost_sum / worker_lower_bound_sum) if worker_lower_bound_sum else 0.0,
        "longest_chain_sum": longest_chain_sum,
        "weighted_longest_chain_cost_sum": weighted_longest_chain_cost_sum,
        "hot_key_worker_lower_bound_cost_sum": hot_key_worker_lower_bound_sum,
        "conflict_critical_path_tx_sum": conflict_cp_tx_sum,
        "total_cost": total_cost_sum,
        "conflict_critical_path_cost_sum": conflict_cp_cost_sum,
        "worker_lower_bound_cost_sum": worker_lower_bound_sum,
    }


def translated_parallelism(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[int, int], list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        if row.get("strategy") != "cosmos-wasmd-vegeta":
            continue
        grouped[(int(row["workers"]), int(row["sample"]))].append(row)

    per_sample: list[dict[str, Any]] = []
    for (workers, sample), rs in sorted(grouped.items()):
        if not any(int(r.get("vegeta_weighted_longest_chain_cost", 0) or 0) > 0 for r in rs):
            # Older campaigns predate the weighted chain/ready-wave instrumentation.
            continue
        txs = sum(int(r.get("transactions", 0)) for r in rs)
        longest = sum(int(r.get("vegeta_longest_chain", 0) or 0) for r in rs)
        weighted_longest = sum(int(r.get("vegeta_weighted_longest_chain_cost", 0) or 0) for r in rs)
        total_cost = sum(int(r.get("vegeta_total_estimated_cost", 0) or 0) for r in rs)
        hot_key_lb = sum(int(r.get("vegeta_hot_key_worker_lower_bound_cost", 0) or 0) for r in rs)
        ready_lb = sum(int(r.get("vegeta_ready_worker_lower_bound_cost", 0) or 0) for r in rs)
        post_work = sum(int(r.get("vegeta_post_exec_work_nanos", 0) or 0) for r in rs)
        post_span = sum(int(r.get("vegeta_post_exec_span_nanos", 0) or 0) for r in rs)
        wide_work = sum(int(r.get("vegeta_post_wide_exec_work_nanos", 0) or 0) for r in rs)
        wide_span = sum(int(r.get("vegeta_post_wide_exec_span_nanos", 0) or 0) for r in rs)
        batches = sum(int(r.get("vegeta_post_batches", 0) or 0) for r in rs)
        singleton = sum(int(r.get("vegeta_post_singleton_batches", 0) or 0) for r in rs)
        per_sample.append({
            "workers": workers,
            "sample": sample,
            "blocks": len(rs),
            "transactions": txs,
            "hot_key_chain_ratio": (txs / longest) if longest else 0.0,
            "weighted_hot_key_parallelism": (total_cost / weighted_longest) if weighted_longest else 0.0,
            "hot_key_ideal_worker_speedup": (total_cost / hot_key_lb) if hot_key_lb else 0.0,
            "ready_wave_ideal_worker_speedup": (total_cost / ready_lb) if ready_lb else 0.0,
            "post_tx_concurrency": (post_work / post_span) if post_span else 0.0,
            "wide_batch_concurrency": (wide_work / wide_span) if wide_span else 0.0,
            "singleton_batch_pct": (100.0 * singleton / batches) if batches else 0.0,
            "max_batch": max((int(r.get("vegeta_post_max_batch", 0) or 0) for r in rs), default=0),
            "longest_chain_sum": longest,
            "weighted_longest_chain_cost_sum": weighted_longest,
            "total_estimated_cost": total_cost,
            "hot_key_worker_lower_bound_cost_sum": hot_key_lb,
            "ready_worker_lower_bound_cost_sum": ready_lb,
        })

    by_workers: dict[int, list[dict[str, Any]]] = defaultdict(list)
    for row in per_sample:
        by_workers[row["workers"]].append(row)
    out: list[dict[str, Any]] = []
    metrics = [
        "hot_key_chain_ratio", "weighted_hot_key_parallelism", "hot_key_ideal_worker_speedup",
        "ready_wave_ideal_worker_speedup", "post_tx_concurrency", "wide_batch_concurrency",
        "singleton_batch_pct",
    ]
    for workers, samples in sorted(by_workers.items()):
        item: dict[str, Any] = {
            "workers": workers,
            "samples": len(samples),
            "blocks": samples[0]["blocks"],
            "transactions": samples[0]["transactions"],
            "max_batch": max(s["max_batch"] for s in samples),
        }
        for metric in metrics:
            item[metric] = statistics.fmean(float(s[metric]) for s in samples)
        out.append(item)
    return out


def build_parallelism_diagnostic(
    rows: list[dict[str, Any]], source_corpus: Path | None, dataset_tag: str | None, cost_metric: str
) -> dict[str, Any] | None:
    translated = translated_parallelism(rows)
    if not translated:
        return None
    paper = None
    if dataset_tag:
        spec = dataset_by_tag(dataset_tag)
        paper = {
            "dataset": spec.tag,
            "blocks": spec.blocks,
            "transactions": spec.paper_transactions,
            "hot_key_chain_ratio": spec.paper_ratio,
            "longest_chain_sum": spec.paper_longest_chain_sum,
        }
    source_by_workers: list[dict[str, Any]] = []
    if source_corpus:
        if not source_corpus.is_file():
            raise SystemExit(f"missing source corpus for workload parallelism: {source_corpus}")
        serial = [r for r in rows if r.get("strategy") == "cosmos-wasmd-direct-serial"]
        if not serial:
            raise SystemExit("cannot derive source-prefix domain without Serial rows")
        block_numbers = {int(r["block_number"]) for r in serial}
        for workers in sorted({int(r["workers"]) for r in serial}):
            metric = source_prefix_parallelism(source_corpus, block_numbers, workers)
            metric["workers"] = workers
            source_by_workers.append(metric)
    return {
        "paper_full_dataset": paper,
        "source_prefix_by_workers": source_by_workers,
        "translated_wasmd_by_workers": translated,
        "cost_metric": cost_metric,
        "definitions": {
            "hot_key_chain_ratio": "transactions / sum(per-block maximum transactions touching one key); directly comparable to Vegeta Table 2 when instrumentation matches",
            "weighted_hot_key_parallelism": f"{cost_metric}-weighted total work / sum(per-block cost of the heaviest single-key dependency chain)",
            "hot_key_ideal_worker_speedup": f"total {cost_metric}-weighted work / sum(max(block_work/workers, heaviest_key_chain_cost)); optimistic hot-key structural estimate",
            "ready_wave_ideal_worker_speedup": f"translated Vegeta total {cost_metric}-weighted work / sum over actual replay-ready waves of max(wave_work/workers, largest_tx_cost); cost-model schedule estimate",
            "source_conflict_dag_parallelism": "source-only historical-order conflict-DAG diagnostic; provided as extra context, not as the direct Table-2 comparison",
            "post_tx_concurrency": "Vegeta measured post-replay transaction execution work / execution span",
        },
    }


def render_parallelism(diag: dict[str, Any] | None) -> list[str]:
    if not diag:
        return []
    lines = ["", "Workload parallelism diagnostic"]
    paper = diag.get("paper_full_dataset")
    if paper:
        lines.append(
            f"  Vegeta paper {paper['dataset']} full dataset: hot-key chain ratio={paper['hot_key_chain_ratio']:.2f}x "
            f"({paper['blocks']} blocks)"
        )
    source = {int(r["workers"]): r for r in diag.get("source_prefix_by_workers", [])}
    cost_metric = str(diag.get("cost_metric") or "cost")
    for translated in diag.get("translated_wasmd_by_workers", []):
        workers = int(translated["workers"])
        src = source.get(workers)
        if src:
            lines += [
                f"  Source Ethereum benchmark prefix ({src['blocks']} blocks / {src['transactions']} tx, w={workers} bound):",
                f"    hot-key={src['hot_key_chain_ratio']:.3f}x  cost-weighted-hot-key={src['weighted_hot_key_parallelism']:.3f}x  "
                f"hot-key-ideal-{workers}w={src['hot_key_ideal_worker_speedup']:.3f}x",
                f"    source historical conflict-DAG={src['conflict_dag_parallelism']:.3f}x  "
                f"cost-weighted-DAG={src['weighted_dag_parallelism']:.3f}x  conflict-DAG-ideal-{workers}w={src['conflict_dag_ideal_worker_speedup']:.3f}x",
            ]
        lines += [
            f"  Translated Wasmd prefix ({translated['blocks']} blocks / {translated['transactions']} tx):",
            f"    hot-key={translated['hot_key_chain_ratio']:.3f}x  cost-weighted-hot-key={translated['weighted_hot_key_parallelism']:.3f}x  "
            f"hot-key-ideal-{workers}w={translated['hot_key_ideal_worker_speedup']:.3f}x",
            f"    ready-wave-cost-model-{workers}w={translated['ready_wave_ideal_worker_speedup']:.3f}x  "
            f"observed Vegeta tx concurrency={translated['post_tx_concurrency']:.3f}x  "
            f"wide-batch concurrency={translated['wide_batch_concurrency']:.3f}x  "
            f"singleton-batches={translated['singleton_batch_pct']:.1f}%  max-batch={translated['max_batch']}",
        ]
        if src and src["hot_key_chain_ratio"]:
            lines.append(
                f"    translated/source hot-key ratio={translated['hot_key_chain_ratio']/src['hot_key_chain_ratio']:.3f}x"
            )
        if src and src["weighted_hot_key_parallelism"]:
            lines.append(
                f"    translated/source cost-weighted-hot-key ratio="
                f"{translated['weighted_hot_key_parallelism']/src['weighted_hot_key_parallelism']:.3f}x"
            )
    lines += [
        f"  Cost metric for weighted diagnostics: {cost_metric}",
        "  Interpretation:",
        "    * A much lower translated hot-key/cost-weighted-hot-key ratio than the source prefix points to workload/translation loss.",
        "    * A high translated ready-wave cost-model estimate but low measured tx concurrency points to local scheduler/executor overhead.",
        "    * Rust-ACG replay-x is intentionally not capped by this worker bound because useful work is moved before consensus.",
    ]
    return lines


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


def parse_consensus_windows_ms(value: str | None) -> list[float]:
    if value is None or not value.strip():
        return list(DEFAULT_CONSENSUS_WINDOWS_MS)
    out: list[float] = []
    for token in value.split(","):
        token = token.strip()
        if not token:
            continue
        try:
            v = float(token)
        except ValueError as exc:
            raise SystemExit(f"invalid consensus window {token!r}") from exc
        if not math.isfinite(v) or v < 0:
            raise SystemExit("consensus windows must be finite non-negative milliseconds")
        out.append(v)
    if not out:
        raise SystemExit("--consensus-windows-ms produced an empty window set")
    return sorted(set(out))


def build_consensus_window_sweep(
    rows: list[dict[str, Any]], strategy_order: list[str], windows_ms: list[float]
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Evaluate externally supplied consensus windows without pretending they were measured.

    P is local pre-consensus planning/speculation for algorithms that can overlap it
    with consensus. R is consensus-visible post-consensus work, including any
    canonical fallback required to obtain committed state. For a constant external
    consensus window C per block:

        tail(C)   = R + max(0, P-C)
        commit(C) = C + tail(C) = max(C, P) + R

    tail-x compares remaining execution-tail time against Serial execution. commit-x
    compares proposal-to-commit time against Serial under the same C. All speedups
    are paired within the same worker/sample campaign.
    """
    grouped: dict[tuple[str, int, int], list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        strategy = row.get("strategy")
        if strategy not in strategy_order:
            continue
        grouped[(strategy, int(row["workers"]), int(row["sample"]))].append(row)
    for rs in grouped.values():
        rs.sort(key=lambda r: int(r["block_number"]))

    per_sample: list[dict[str, Any]] = []
    for (strategy, workers, sample), rs in sorted(grouped.items()):
        serial_rs = grouped.get(("cosmos-wasmd-direct-serial", workers, sample))
        if not serial_rs:
            raise SystemExit(f"missing Serial rows for consensus sweep workers={workers} sample={sample}")
        txs = sum(int(r.get("transactions", 0)) for r in rs)
        blocks = len(rs)
        serial_post_ns = sum(record_post_consensus_nanos(r) for r in serial_rs)
        pre_eligible = strategy in PRECONSENSUS
        pre_values = [int(r.get("pre_consensus_nanos", 0)) if pre_eligible else 0 for r in rs]
        post_values = [record_post_consensus_nanos(r) for r in rs]
        total_pre_ns = sum(pre_values)

        for c_ms in windows_ms:
            c_ns = int(round(c_ms * 1e6))
            overruns = [max(0, p - c_ns) for p in pre_values]
            tail_ns = sum(post_values) + sum(overruns)
            commit_ns = sum(max(c_ns, p) + r for p, r in zip(pre_values, post_values))
            serial_tail_ns = serial_post_ns
            serial_commit_ns = blocks * c_ns + serial_post_ns
            covered = sum(1 for p in pre_values if p <= c_ns)
            hidden_ns = sum(min(p, c_ns) for p in pre_values)
            per_sample.append({
                "strategy": strategy,
                "label": LABELS.get(strategy, strategy),
                "workers": workers,
                "sample": sample,
                "blocks": blocks,
                "transactions": txs,
                "consensus_window_ms": c_ms,
                "overlap_tail_ms": tail_ns / 1e6,
                "overlap_tail_tps": (txs * 1e9 / tail_ns) if tail_ns else 0.0,
                "overlap_tail_x": (serial_tail_ns / tail_ns) if tail_ns else 0.0,
                "commit_ms": commit_ns / 1e6,
                "commit_tps": (txs * 1e9 / commit_ns) if commit_ns else 0.0,
                "commit_x": (serial_commit_ns / commit_ns) if commit_ns else 0.0,
                "pre_coverage_pct": (100.0 * covered / blocks) if blocks else 0.0,
                "pre_hidden_fraction": (hidden_ns / total_pre_ns) if total_pre_ns else 1.0,
                "pre_overrun_ms": sum(overruns) / 1e6,
                "pre_overrun_per_block_ms": (sum(overruns) / blocks / 1e6) if blocks else 0.0,
            })

    metrics = [
        "overlap_tail_ms", "overlap_tail_tps", "overlap_tail_x",
        "commit_ms", "commit_tps", "commit_x", "pre_coverage_pct",
        "pre_hidden_fraction", "pre_overrun_ms", "pre_overrun_per_block_ms",
    ]
    grouped_samples: dict[tuple[str, int, float], list[dict[str, Any]]] = defaultdict(list)
    for row in per_sample:
        grouped_samples[(row["strategy"], row["workers"], row["consensus_window_ms"])].append(row)
    aggregate: list[dict[str, Any]] = []
    order = {s: i for i, s in enumerate(STRATEGY_ORDER)}
    for (strategy, workers, c_ms), samples in grouped_samples.items():
        item: dict[str, Any] = {
            "strategy": strategy,
            "label": LABELS.get(strategy, strategy),
            "workers": workers,
            "samples": len(samples),
            "blocks": samples[0]["blocks"],
            "transactions": samples[0]["transactions"],
            "consensus_window_ms": c_ms,
        }
        for metric in metrics:
            vals = [float(s[metric]) for s in samples]
            mean, ci = mean_ci95(vals)
            item[metric] = mean
            item[f"{metric}_ci95"] = ci
            item[f"{metric}_median"] = statistics.median(vals)
        aggregate.append(item)
    aggregate.sort(key=lambda r: (r["consensus_window_ms"], r["workers"], order.get(r["strategy"], 999)))
    per_sample.sort(key=lambda r: (r["consensus_window_ms"], r["workers"], r["sample"], order.get(r["strategy"], 999)))
    return per_sample, aggregate


def build_acg_vs_best_baseline(per_sample_sweep: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    by_key: dict[tuple[int, int, float], dict[str, dict[str, Any]]] = defaultdict(dict)
    for row in per_sample_sweep:
        by_key[(int(row["workers"]), int(row["sample"]), float(row["consensus_window_ms"]))][row["strategy"]] = row
    baseline_strategies = [
        "cosmos-wasmd-direct-serial", "cosmos-wasmd-block-stm",
        "cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta",
    ]
    per_sample: list[dict[str, Any]] = []
    for (workers, sample, c_ms), rows in sorted(by_key.items()):
        acg = rows.get("cosmos-wasmd-symbgraph-rust")
        baselines = [rows[s] for s in baseline_strategies if s in rows]
        if not acg or not baselines:
            continue
        best = max(baselines, key=lambda r: float(r["commit_tps"]))
        base_tps = float(best["commit_tps"])
        acg_tps = float(acg["commit_tps"])
        per_sample.append({
            "workers": workers,
            "sample": sample,
            "consensus_window_ms": c_ms,
            "best_baseline": best["label"],
            "acg_vs_best_commit_x": (acg_tps / base_tps) if base_tps else 0.0,
            "acg_commit_x": float(acg["commit_x"]),
            "best_baseline_commit_x": float(best["commit_x"]),
        })
    grouped: dict[tuple[int, float], list[dict[str, Any]]] = defaultdict(list)
    for row in per_sample:
        grouped[(int(row["workers"]), float(row["consensus_window_ms"]))].append(row)
    aggregate: list[dict[str, Any]] = []
    for (workers, c_ms), rows in sorted(grouped.items()):
        vals = [float(r["acg_vs_best_commit_x"]) for r in rows]
        mean, ci = mean_ci95(vals)
        # Baseline identity may vary across samples; expose the modal label only as context.
        labels = [str(r["best_baseline"]) for r in rows]
        modal = max(sorted(set(labels)), key=labels.count)
        aggregate.append({
            "workers": workers,
            "samples": len(rows),
            "consensus_window_ms": c_ms,
            "best_baseline": modal,
            "acg_vs_best_commit_x": mean,
            "acg_vs_best_commit_x_ci95": ci,
            "acg_vs_best_commit_x_median": statistics.median(vals),
        })
    return per_sample, aggregate


def build_overlap_break_even(per_sample_sweep: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Grid-based ACG break-even versus the best deployable baseline at each C."""
    by_key: dict[tuple[int, int, float], dict[str, dict[str, Any]]] = defaultdict(dict)
    for row in per_sample_sweep:
        by_key[(int(row["workers"]), int(row["sample"]), float(row["consensus_window_ms"]))][row["strategy"]] = row
    samples = sorted({(w, s) for w, s, _ in by_key})
    windows = sorted({c for _, _, c in by_key})
    baseline_strategies = [
        "cosmos-wasmd-direct-serial", "cosmos-wasmd-block-stm",
        "cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta",
    ]
    out: list[dict[str, Any]] = []
    for workers, sample in samples:
        found = None
        for c_ms in windows:
            rows = by_key.get((workers, sample, c_ms), {})
            acg = rows.get("cosmos-wasmd-symbgraph-rust")
            baselines = [rows[s] for s in baseline_strategies if s in rows]
            if not acg or not baselines:
                continue
            best = max(baselines, key=lambda r: float(r["commit_tps"]))
            if float(acg["commit_tps"]) >= float(best["commit_tps"]):
                found = {
                    "workers": workers, "sample": sample,
                    "break_even_grid_ms": c_ms,
                    "best_baseline": best["label"],
                    "acg_commit_x": float(acg["commit_x"]),
                    "baseline_commit_x": float(best["commit_x"]),
                }
                break
        if found is not None:
            out.append(found)
    grouped: dict[int, list[dict[str, Any]]] = defaultdict(list)
    for row in out:
        grouped[int(row["workers"])].append(row)
    aggregate: list[dict[str, Any]] = []
    for workers, rows in sorted(grouped.items()):
        vals = [float(r["break_even_grid_ms"]) for r in rows]
        mean, ci = mean_ci95(vals)
        aggregate.append({
            "workers": workers,
            "samples_with_break_even": len(rows),
            "break_even_grid_ms": mean,
            "break_even_grid_ms_ci95": ci,
            "break_even_grid_ms_median": statistics.median(vals),
        })
    return out, aggregate


def build_optimal_workers(aggregate_sweep: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Choose the measured worker count with best modeled commit throughput at each C."""
    by_strategy_window: dict[tuple[str, float], list[dict[str, Any]]] = defaultdict(list)
    for row in aggregate_sweep:
        if row["strategy"] not in PRECONSENSUS:
            continue
        by_strategy_window[(row["strategy"], float(row["consensus_window_ms"]))].append(row)
    out: list[dict[str, Any]] = []
    for (strategy, c_ms), rows in sorted(by_strategy_window.items(), key=lambda x: (x[0][0], x[0][1])):
        best = max(rows, key=lambda r: float(r["commit_tps"]))
        out.append({
            "strategy": strategy,
            "label": LABELS.get(strategy, strategy),
            "consensus_window_ms": c_ms,
            "optimal_workers": int(best["workers"]),
            "commit_tps": float(best["commit_tps"]),
            "commit_x": float(best["commit_x"]),
            "overlap_tail_x": float(best["overlap_tail_x"]),
            "pre_coverage_pct": float(best["pre_coverage_pct"]),
        })
    return out



def attach_canonical_consensus_metrics(
    rows: list[dict[str, Any]],
    sweep: list[dict[str, Any]],
    windows_ms: list[float],
) -> None:
    """Mirror the single canonical C into ordinary summary/per-sample rows.

    Normal paper experiments use exactly one externally fixed C (300 ms by
    default). Keeping those overlap-aware metrics only in consensus-sweep.csv
    made the main tables easy to misread, so mirror the canonical point into
    summary.csv/summary.json and per-sample.csv. Dedicated sensitivity runs
    intentionally retain a multi-C sweep and therefore do not choose one point.
    """
    if len(windows_ms) != 1:
        return
    c_ms = windows_ms[0]
    per_sample = bool(rows and "sample" in rows[0])

    def key(r: dict[str, Any]) -> tuple[Any, ...]:
        base: tuple[Any, ...] = (str(r["strategy"]), int(r["workers"]))
        if per_sample:
            return base + (int(r["sample"]),)
        return base

    by_key = {
        key(r): r
        for r in sweep
        if abs(float(r.get("consensus_window_ms", -1.0)) - c_ms) < 1e-9
    }
    metrics = [
        "overlap_tail_ms", "overlap_tail_tps", "overlap_tail_x",
        "commit_ms", "commit_tps", "commit_x",
        "pre_coverage_pct", "pre_hidden_fraction",
        "pre_overrun_ms", "pre_overrun_per_block_ms",
    ]
    for row in rows:
        fixed = by_key.get(key(row))
        if fixed is None:
            continue
        row["consensus_window_ms"] = c_ms
        for metric in metrics:
            row[metric] = fixed.get(metric, 0.0)
            if not per_sample:
                row[f"{metric}_ci95"] = fixed.get(f"{metric}_ci95", 0.0)
                row[f"{metric}_median"] = fixed.get(f"{metric}_median", fixed.get(metric, 0.0))

def validate_campaign_completeness(rows: list[dict[str, Any]], strategy_order: list[str]) -> None:
    """Require an identical block/transaction campaign for every strategy.

    Serial is the canonical key set for each (workers, sample). Publication
    summaries must never silently compare partial strategy output against a
    complete serial run.
    """
    keyed: dict[tuple[str, int, int], dict[int, int]] = defaultdict(dict)
    samples: set[tuple[int, int]] = set()
    for row in rows:
        strategy = row.get("strategy")
        if strategy not in strategy_order:
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
        for strategy in strategy_order:
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
        "throughput_tps", "throughput_speedup",
        "post_ms", "post_p50_ms", "post_p95_ms", "post_p99_ms", "wall_ms",
        "pre_p50_ms", "pre_p95_ms", "pre_p99_ms", "pre_max_ms", "reexec_pct",
        "reexec_ms", "conflict_analysis_ms",
        "aria_historical_fallback_ms",
        "vegeta_intrinsic_reexecution_ms", "vegeta_historical_fallback_ms",
        "structural_parallelism", "source_trace_missing", "translation_compensation_edges",
    ]
    out: list[dict[str, Any]] = []
    for (strategy, workers), samples in grouped.items():
        samples = sorted(samples, key=lambda r: r["sample"])
        row: dict[str, Any] = {
            "strategy": strategy,
            "label": LABELS.get(strategy, strategy),
            "workers": workers,
            "samples": len(samples),
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
        row["structural_edges"] = statistics.fmean(float(s["structural_edges"]) for s in samples)
        out.append(row)
    order = {s: i for i, s in enumerate(STRATEGY_ORDER)}
    out.sort(key=lambda r: (r["workers"], order.get(r["strategy"], 999), r["strategy"]))
    return out


def build_per_sample(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
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
        wall_ns = sum(int(r.get("strategy_total_nanos", 0)) for r in rs)
        post_values = [record_post_consensus_nanos(r) for r in rs]
        post_ns = sum(post_values)
        pre_values = [int(r.get("pre_consensus_nanos", 0)) for r in rs]
        replay_exec_ns = sum(int(r.get("replay_execution_nanos", 0)) for r in rs)
        conflict_ns = sum(int(r.get("conflict_analysis_nanos", 0)) for r in rs)
        aria_historical_fallback_ns = sum(int(r.get("aria_historical_fallback_nanos", 0)) for r in rs)
        vegeta_intrinsic_ns = sum(int(r.get("vegeta_intrinsic_reexecution_nanos", 0)) for r in rs)
        vegeta_historical_fallback_ns = sum(int(r.get("vegeta_historical_fallback_nanos", 0)) for r in rs)
        reexec = sum(int(r.get("reexecutions", 0)) for r in rs)
        forward = sum(int(r.get("forward_fallbacks", 0)) for r in rs)
        safety = sum(int(r.get("safety_replays", 0)) for r in rs)
        source_trace_missing = sum(int(r.get("oracle_source_trace_missing", 0)) for r in rs)
        translation_compensation_edges = sum(int(r.get("oracle_translation_compensation_edges", 0)) for r in rs)
        symb_edges = sum(int(r.get("symb_dependency_edges", 0)) for r in rs)
        symb_total_cost = sum(int(r.get("symb_total_estimated_cost", 0)) for r in rs)
        symb_cp_cost = sum(int(r.get("symb_critical_path_cost", 0)) for r in rs)
        structural_parallelism = 0.0
        structural_edges = 0
        if strategy in {"cosmos-wasmd-symbgraph-rust", "cosmos-wasmd-symbgraph-rust-exact-trace-oracle"} and symb_cp_cost:
            structural_parallelism = symb_total_cost / symb_cp_cost
            structural_edges = symb_edges
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
            # Vegeta NSDI'25 Figure 10 single-node throughput measures the replay
            # phase only; speculation is pre-consensus/pipelined. Use that same
            # consensus-visible execution definition for every system so the
            # primary throughput column is directly comparable: Serial's post is
            # ordinary serial execution, while SOR systems' post is replay plus
            # validation/re-execution actually required after ordering.
            "throughput_tps": (txs * 1e9 / post_ns) if post_ns else 0.0,
            "post_ms": post_ns / 1e6,
            "post_p50_ms": percentile([x / 1e6 for x in post_values], 0.50),
            "post_p95_ms": percentile([x / 1e6 for x in post_values], 0.95),
            "post_p99_ms": percentile([x / 1e6 for x in post_values], 0.99),
            "wall_ms": wall_ns / 1e6,
            "pre_p50_ms": percentile([x / 1e6 for x in pre_values], 0.50),
            "pre_p95_ms": percentile([x / 1e6 for x in pre_values], 0.95),
            "pre_p99_ms": percentile([x / 1e6 for x in pre_values], 0.99),
            "pre_max_ms": max(pre_values, default=0) / 1e6,
            "reexec_ms": replay_exec_ns / 1e6,
            "conflict_analysis_ms": conflict_ns / 1e6,
            "aria_historical_fallback_ms": aria_historical_fallback_ns / 1e6,
            "vegeta_intrinsic_reexecution_ms": vegeta_intrinsic_ns / 1e6,
            "vegeta_historical_fallback_ms": vegeta_historical_fallback_ns / 1e6,
            "structural_parallelism": structural_parallelism,
            "source_trace_missing": float(source_trace_missing),
            "translation_compensation_edges": float(translation_compensation_edges),
            "structural_edges": structural_edges,
            "reexecutions": reexec,
            "reexec_pct": (100.0 * reexec / txs) if txs else 0.0,
            "forward_fallbacks": forward,
            "safety_replays": safety,
            "serial_equivalent": serial_equivalent,
        })

    serial_by_sample = {
        (r["workers"], r["sample"]): r
        for r in out if r["strategy"] == "cosmos-wasmd-direct-serial"
    }
    for row in out:
        serial = serial_by_sample.get((row["workers"], row["sample"]))
        serial_tps = float(serial["throughput_tps"]) if serial else 0.0
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


def render_preconsensus(rows: list[dict[str, Any]], windows_ms: list[float]) -> list[str]:
    relevant = [r for r in rows if r.get("strategy") in PRECONSENSUS]
    if not relevant:
        return []
    lines = [
        "",
        "Pre-consensus timing diagnostics",
        "  P = local planning/speculation/preexecution; R = consensus-visible post-consensus work.",
        "  For an external consensus window C: tail(C)=R+max(0,P-C), commit(C)=max(C,P)+R.",
        "  tail-x(C) compares the remaining execution tail to Serial; commit-x(C) compares proposal-to-commit time under the same C.",
        "  C is an externally fixed model parameter in this single-node harness, not measured consensus latency.",
        (f"  canonical-consensus-window-ms={windows_ms[0]:g}" if len(windows_ms) == 1 else f"  sensitivity-sweep-ms={','.join(f'{v:g}' for v in windows_ms)}"),
        f"{'system':<12} {'w':>3} {'pre-p50':>10} {'pre-p95':>10} {'pre-max':>10}",
    ]
    for r in relevant:
        lines.append(
            f"{r['label']:<12} {r['workers']:>3} {r['pre_p50_ms']:>10.2f} "
            f"{r['pre_p95_ms']:>10.2f} {r['pre_max_ms']:>10.2f}"
        )
    if len(windows_ms) == 1:
        lines.append("  Fixed-window overlap metrics: summary/consensus-sweep.csv")
    else:
        lines += [
            "  Full sensitivity: summary/consensus-sweep.csv",
            "  ACG grid break-even: summary/consensus-break-even.csv",
            "  Worker choice under each C: summary/consensus-optimal-workers.csv",
        ]
    return lines


def render_consensus_metrics(sweep: list[dict[str, Any]], windows_ms: list[float]) -> list[str]:
    if not sweep:
        return []
    if len(windows_ms) != 1:
        return [
            "",
            "Consensus-window sensitivity",
            f"  sweep-ms={','.join(f'{v:g}' for v in windows_ms)}",
            "  See consensus-sweep.csv / consensus-acg-vs-best.csv / consensus-break-even.csv.",
        ]
    c = windows_ms[0]
    return [
        "",
        f"Canonical overlap model: C={c:g} ms",
        "  tail-x / commit-x / coverage are reported directly in the primary table and summary.csv.",
        "  Detailed hidden-work and overrun accounting remains in consensus-sweep.csv.",
        "  C is a fixed external design point, not a measured consensus latency.",
    ]


def render(
    rows: list[dict[str, Any]], rust_acg_only: bool = False, exact_oracle: bool = True,
    parallelism: dict[str, Any] | None = None, windows_ms: list[float] | None = None,
    consensus_sweep: list[dict[str, Any]] | None = None,
) -> str:
    lines = [
        "Wasmd controlled scheduler evaluation",
        "",
        "Replay throughput (Vegeta NSDI'25 single-node comparability):",
        "  replay-tps = transactions / consensus-visible post phase.",
        "  replay-x   = replay-tps / Serial replay-tps (common historical Serial baseline).",
        "  Pre-consensus speculation/planning is excluded; required post-order validation/re-execution/canonical fallback is included.",
        "  Normal paper runs also mirror tail-x / commit-x / coverage for the canonical fixed consensus window into this table.",
        "",
        (
            f"{'system':<12} {'w':>3} {'n':>3} {'replay-tps':>11} {'replay-x':>8} "
            f"{'tail-x':>8} {'commit-x':>9} {'cover-%':>8} {'post-ms':>10} "
            f"{'reexec-%':>9} {'reexec-ms':>10}"
        ),
    ]
    fixed_metrics = all("overlap_tail_x" in r and "commit_x" in r for r in rows)
    if not fixed_metrics:
        # Sensitivity-only summaries have no single canonical C to mirror into
        # the primary table; keep their replay table unambiguous.
        lines[-1] = f"{'system':<12} {'w':>3} {'n':>3} {'replay-tps':>11} {'replay-x':>8} {'post-ms':>10} {'reexec-%':>9} {'reexec-ms':>10}"
    for r in rows:
        if fixed_metrics:
            lines.append(
                f"{r['label']:<12} {r['workers']:>3} {r['samples']:>3} "
                f"{r['throughput_tps']:>11.1f} {r['throughput_speedup']:>8.2f} "
                f"{float(r['overlap_tail_x']):>8.2f} {float(r['commit_x']):>9.2f} "
                f"{float(r['pre_coverage_pct']):>7.1f}% {r['post_ms']:>10.1f} "
                f"{r['reexec_pct']:>8.2f}% {r['reexec_ms']:>10.1f}"
            )
        else:
            lines.append(
                f"{r['label']:<12} {r['workers']:>3} {r['samples']:>3} "
                f"{r['throughput_tps']:>11.1f} {r['throughput_speedup']:>8.2f} "
                f"{r['post_ms']:>10.1f} {r['reexec_pct']:>8.2f}% "
                f"{r['reexec_ms']:>10.1f}"
            )
    if exact_oracle:
        by_worker = defaultdict(dict)
        for r in rows:
            by_worker[r["workers"]][r["strategy"]] = r
        lines += [
            "",
            "Rust-ACG perfect-access headroom",
            f"{'w':>3} {'oracle-tps':>11} {'acg-tps':>10} {'oracle/acg':>10} {'oracle-dag':>11} {'acg-dag':>9} {'native+':>8} {'missing':>8}",
        ]
        for workers in sorted(by_worker):
            oracle = by_worker[workers].get("cosmos-wasmd-symbgraph-rust-exact-trace-oracle")
            acg = by_worker[workers].get("cosmos-wasmd-symbgraph-rust")
            if not oracle or not acg:
                continue
            acg_tps = float(acg.get("throughput_tps", 0.0))
            oracle_tps = float(oracle.get("throughput_tps", 0.0))
            gap = oracle_tps / acg_tps if acg_tps else 0.0
            lines.append(
                f"{workers:>3} {oracle_tps:>11.1f} {acg_tps:>10.1f} {gap:>10.3f} "
                f"{oracle.get('structural_parallelism', 0.0):>11.2f} {acg.get('structural_parallelism', 0.0):>9.2f} "
                f"{oracle.get('translation_compensation_edges', 0.0):>8.0f} {oracle.get('source_trace_missing', 0.0):>8.0f}"
            )
    resolved_windows = windows_ms or list(DEFAULT_CONSENSUS_WINDOWS_MS)
    lines += render_preconsensus(rows, resolved_windows)
    lines += render_consensus_metrics(consensus_sweep or [], resolved_windows)
    lines += render_parallelism(parallelism)
    lines += [
        "",
        "Reporting notes:",
        "  * replay-tps / replay-x are the publication-facing Vegeta-paper replay-throughput metrics.",
        "  * reexec-% is the fraction of transactions repeated by the strategy's intrinsic fallback/re-execution path.",
        "  * reexec-ms is the measured time in that intrinsic fallback/re-execution path where the runner exposes it.",
        "  * validation timers remain in raw JSONL diagnostics but are intentionally omitted from the publication table.",
        "  * pre_consensus_nanos records local speculation/planning duration; this single-node harness does not measure consensus decision latency.",
        "  * overlap accounting is external: tail(C)=R+max(0,P-C) and commit(C)=max(C,P)+R; no measured consensus latency is implied.",
        "  * normal paper experiments use one canonical fixed C; a separate S1 sensitivity experiment sweeps C.",
        "  * replay-x is retained for Vegeta comparability; tail-x(C) is the overlap-aware execution metric and commit-x(C) is the proposal-to-commit model metric.",
    ]
    if not rust_acg_only:
        lines += [
            "  * AriaFB ports the attached repository's exact Rule-2 abort condition and completion-driven hot-chain DAG fallback to Wasmd (successors launch as soon as their last predecessor completes).",
            "  * AriaFB replay-tps includes its initial post-consensus batch, Rule-2 analysis/fallback, intrinsic Wasmd safety replay, and any whole-block canonical restoration required to commit historical state; the restoration remains separately reported.",
            "  * Vegeta ports Algorithm 1 full longest-to-shortest dependency-chain ordering, BuildDAG dependency classes, Rule-2 replay batches, and access-change handling to Wasmd.",
            "  * Vegeta replay-tps includes Algorithm-3 validation, intrinsic re-execution, and any historical-state fallback required by this fixed-history evaluator; intrinsic and evaluator-required fallback counters remain separate.",
        ]
    if exact_oracle:
        lines += [
            "  * ACG-Oracle replaces symbolic prediction with the frozen exact Ethereum SLOAD/SSTORE trace and materializes its minimal RAW visibility dependencies. Because the native Wasmd translation can alias multiple source states or introduce read-modify-write behavior, the frozen native-translation access audit contributes only the additional RAW edges absent from the Ethereum relation; adapter bank/funds hard resources remain unchanged.",
            "  * The native-translation compensation is frozen before the scheduler campaign and is not a per-run Wasmd access-discovery pass. It exists solely to make the perfect-source oracle faithful to the workload actually executed after EVM->CosmWasm translation.",
            "  * ACG-Oracle is required to finish with zero replay and historical-order state equivalence; any replay aborts the campaign instead of weakening the claimed upper bound.",
            "  * Missing exact source traces are conservative serial barriers and are reported in the headroom table; they make the oracle slightly pessimistic rather than optimistic.",
            "  * oracle/acg in the headroom table is the throughput gain still available if current symbolic access extraction became perfect under the same validation design.",
        ]
    lines += [
        "  * serial_equivalent and matched_serial_nanos remain in machine-readable artifacts for correctness diagnostics; historical_serial_nanos retains the common historical-order control.",
        "  * safety_replays are conservative Wasmd-only fallbacks for dynamic key/range changes absent from the Ethereum access model.",
    ]
    return "\n".join(lines) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--records", required=True, type=Path)
    ap.add_argument("--output-dir", required=True, type=Path)
    ap.add_argument(
        "--rust-acg-only",
        action="store_true",
        help="summarize the direct-serial + ACG-Oracle + Rust-ACG diagnostic subset",
    )
    ap.add_argument(
        "--no-exact-oracle",
        action="store_true",
        help="summarize the deployable five-system campaign without ACG-Oracle (for large datasets without exact SLOAD/SSTORE traces)",
    )
    ap.add_argument("--source-corpus", type=Path, help="optional source Ethereum corpus used for prefix parallelism diagnostics")
    ap.add_argument("--vegeta-dataset-tag", help="optional Vegeta dataset tag (for the published full-dataset chain-ratio reference)")
    ap.add_argument("--cost-metric", default="cost", help="name of the cost proxy used by weighted workload diagnostics (e.g. gas_used or opcode_steps)")
    ap.add_argument("--consensus-windows-ms", default=str(DEFAULT_CONSENSUS_WINDOW_MS), help="external consensus-window point(s) in milliseconds; normal paper runs use the canonical fixed point and the dedicated sensitivity experiment supplies a comma-separated sweep")
    args = ap.parse_args()

    rows = read_jsonl(args.records)
    if args.rust_acg_only and args.no_exact_oracle:
        raise SystemExit("--rust-acg-only requires ACG-Oracle; do not combine with --no-exact-oracle")
    if args.rust_acg_only:
        strategy_order = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
            "cosmos-wasmd-symbgraph-rust",
        ]
    elif args.no_exact_oracle:
        strategy_order = [s for s in STRATEGY_ORDER if s != "cosmos-wasmd-symbgraph-rust-exact-trace-oracle"]
    else:
        strategy_order = STRATEGY_ORDER
    required = set(strategy_order)
    present = {r.get("strategy") for r in rows}
    missing = sorted(required - present)
    if missing:
        raise SystemExit(f"missing Wasmd strategy rows: {', '.join(missing)}")
    if not all(bool(r.get("serial_equivalent", False)) for r in rows if r.get("strategy") in required):
        raise SystemExit("one or more Wasmd records failed serial state equivalence")
    validate_campaign_completeness(rows, strategy_order)

    windows_ms = parse_consensus_windows_ms(args.consensus_windows_ms)
    per_sample = build_per_sample(rows)
    aggregated = aggregate_samples(per_sample)
    sweep_per_sample, sweep = build_consensus_window_sweep(rows, strategy_order, windows_ms)
    attach_canonical_consensus_metrics(per_sample, sweep_per_sample, windows_ms)
    attach_canonical_consensus_metrics(aggregated, sweep, windows_ms)
    acg_advantage_per_sample, acg_advantage = build_acg_vs_best_baseline(sweep_per_sample)
    break_even_per_sample, break_even = build_overlap_break_even(sweep_per_sample)
    optimal_workers = build_optimal_workers(sweep)
    parallelism = build_parallelism_diagnostic(rows, args.source_corpus, args.vegeta_dataset_tag, args.cost_metric)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    write_csv(args.output_dir / "per-sample.csv", per_sample)
    write_csv(args.output_dir / "summary.csv", aggregated)
    write_csv(args.output_dir / "consensus-sweep-per-sample.csv", sweep_per_sample)
    write_csv(args.output_dir / "consensus-sweep.csv", sweep)
    write_csv(args.output_dir / "consensus-acg-vs-best-per-sample.csv", acg_advantage_per_sample)
    write_csv(args.output_dir / "consensus-acg-vs-best.csv", acg_advantage)
    write_csv(args.output_dir / "consensus-break-even-per-sample.csv", break_even_per_sample)
    write_csv(args.output_dir / "consensus-break-even.csv", break_even)
    write_csv(args.output_dir / "consensus-optimal-workers.csv", optimal_workers)
    obj = {
        "schema_version": 2,
        "exact_oracle_enabled": not args.no_exact_oracle,
        "throughput_definition": {
            "primary_all_strategies": "transactions / sum(post_consensus_nanos)",
            "primary_reference": "Vegeta NSDI'25 single-node methodology: compare consensus-visible post-order throughput against Serial execution; pre-consensus speculation is excluded, while evaluator-required canonical restoration is charged to post-order work.",
            "interpretation": (
                "ACG-Oracle uses exact source accesses plus frozen translation-only RAW compensation, but the same ACG execution/validation design."
                if not args.no_exact_oracle
                else "Deployable five-system campaign with no hindsight exact-access oracle."
            ),
        },
        "rows": aggregated,
        "per_sample": per_sample,
        "consensus_overlap_model": {
            "windows_ms": windows_ms,
            "pre_phase": "P = pre_consensus_nanos only for algorithms whose work can overlap consensus",
            "post_phase": "R = post_consensus_nanos",
            "tail": "R + max(0, P-C)",
            "overlap_tail_x": "Serial R / strategy tail(C)",
            "commit": "max(C,P) + R per block",
            "commit_x": "Serial commit(C) / strategy commit(C)",
            "coverage": "fraction of blocks with P <= C",
            "canonical_window_ms": windows_ms[0] if len(windows_ms) == 1 else None,
            "warning": "C is an external model parameter; this single-node harness does not measure consensus latency. Normal paper runs use a fixed canonical C; only the dedicated sensitivity experiment sweeps it.",
        },
        "consensus_sweep": sweep,
        "consensus_acg_vs_best_baseline": acg_advantage,
        "consensus_break_even": break_even,
        "consensus_optimal_workers": optimal_workers,
        "workload_parallelism": parallelism,
    }
    (args.output_dir / "summary.json").write_text(json.dumps(obj, indent=2) + "\n", encoding="utf-8")
    text = render(aggregated, args.rust_acg_only, not args.no_exact_oracle, parallelism, windows_ms, sweep)
    (args.output_dir / "summary.txt").write_text(text, encoding="utf-8")
    print(text, end="")


if __name__ == "__main__":
    main()
