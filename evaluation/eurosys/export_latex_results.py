#!/usr/bin/env python3
"""Export a small set of paper-facing result macros from frozen summaries.

The intent is to keep the evaluation prose stable between the local mock-up and
the final cluster campaign: rerunning postprocess.sh refreshes the numbers while
the surrounding paper text remains unchanged.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import re
from pathlib import Path


def read_csv(path: Path):
    if not path.is_file():
        return []
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def read_json(path: Path):
    if not path.is_file():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def num(row, key, default=0.0):
    if row is None:
        return default
    try:
        value = row.get(key)
        return float(value) if value not in (None, "") else default
    except (TypeError, ValueError):
        return default


def max_workers(rows):
    return max((int(num(row, "workers")) for row in rows), default=0)


def row_for(rows, label, workers):
    return next(
        (row for row in rows if row.get("label") == label and int(num(row, "workers")) == workers),
        None,
    )


def macro(name, value):
    return f"\\newcommand{{\\{name}}}{{{value}}}"


def x(value, digits=2):
    return f"\\ensuremath{{{value:.{digits}f}\\times}}"


def pct(value, digits=2):
    return f"\\ensuremath{{{value:.{digits}f}\\%}}"


def ms(value, digits=0):
    return f"\\ensuremath{{{value:.{digits}f}\\,\\mathrm{{ms}}}}"


def rss_overhead_pct(root: Path, subdir: str, workers: int):
    rows = read_csv(root / subdir / "summary/resource-usage.csv")
    serial = next(
        (num(row, "max_rss_kib") for row in rows if row.get("strategy") == "cosmos-wasmd-direct-serial" and int(num(row, "workers")) == workers),
        None,
    )
    ours = next(
        (num(row, "max_rss_kib") for row in rows if row.get("strategy") == "cosmos-wasmd-symbgraph-rust" and int(num(row, "workers")) == workers),
        None,
    )
    if not serial or ours is None:
        return math.nan
    return 100.0 * (ours / serial - 1.0)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--result-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.result_root

    s1 = read_csv(root / "01-s1/summary/summary.csv")
    s4 = read_csv(root / "02-s4/summary/summary.csv")
    econ = read_csv(root / "eurosys-summary/economics-summary.csv")

    values = {}
    for dataset, rows in [("SOne", s1), ("SFour", s4)]:
        workers = max_workers(rows)
        ours = row_for(rows, "Rust-ACG", workers)
        vegeta = row_for(rows, "Vegeta", workers)
        erows = [row for row in econ if row.get("dataset") == ("S1" if dataset == "SOne" else "S4")]
        eours = row_for(erows, "Rust-ACG", workers)
        values.update(
            {
                f"{dataset}Workers": str(workers),
                f"{dataset}PostSpeedup": x(num(ours, "throughput_speedup")),
                f"{dataset}TailSpeedup": x(num(ours, "overlap_tail_x")),
                f"{dataset}CommitSpeedup": x(num(ours, "commit_x"), 3),
                f"{dataset}VegetaTailSpeedup": x(num(vegeta, "overlap_tail_x")),
                f"{dataset}VegetaCommitSpeedup": x(num(vegeta, "commit_x"), 3),
                f"{dataset}ReusePct": pct(num(eours, "reuse_tx_pct")),
                f"{dataset}ReplayPct": pct(num(eours, "reexec_pct")),
                f"{dataset}AttemptAmplification": x(num(eours, "attempt_amplification"), 3),
                f"{dataset}PreCoveragePct": pct(num(eours, "pre_coverage_pct")),
                f"{dataset}LocalWork": x(num(eours, "local_elapsed_vs_serial"), 3),
            }
        )
        rss = rss_overhead_pct(root, "01-s1" if dataset == "SOne" else "02-s4", workers)
        if math.isfinite(rss):
            values[f"{dataset}RssOverheadPct"] = pct(rss, 1)

    native = read_csv(root / "04-native/native-mix/summary/summary.csv")
    if native:
        workers = max_workers(native)
        ours = row_for(native, "Rust-ACG", workers)
        values["NativeMixTailSpeedup"] = x(num(ours, "overlap_tail_x"))
        values["NativeMixCommitSpeedup"] = x(num(ours, "commit_x"), 3)

    s3 = read_csv(root / "03-s3-breakdown/summary/summary.csv")
    if s3:
        workers = max_workers(s3)
        ours = row_for(s3, "Rust-ACG", workers)
        oracle = row_for(s3, "ACG-Oracle", workers)
        values["SThreePredictedPostMs"] = ms(num(ours, "post_ms"))
        values["SThreeOraclePostMs"] = ms(num(oracle, "post_ms"))
        values["SThreePredictedCommitSpeedup"] = x(num(ours, "commit_x"), 3)
        values["SThreeOracleCommitSpeedup"] = x(num(oracle, "commit_x"), 3)

    upper = read_json(root / "05-upper-bound/upper-bound-report.json").get("rows", [])
    if upper:
        workers = max_workers(upper)
        ours = row_for(upper, "Rust-ACG", workers)
        blockstm = row_for(upper, "BlockSTM", workers)
        values["ZeroConflictWorkers"] = str(workers)
        values["ZeroConflictOursScale"] = x(num(ours, "acg_preexec_scale_vs_1w"))
        values["ZeroConflictBlockSTMScale"] = x(num(blockstm, "post_scale_vs_1w"))

    topology = read_json(root / "16-translation-fidelity/topology/native-topology-fidelity.json")
    pairs = topology.get("conflict_pairs", {}) if isinstance(topology, dict) else {}
    if pairs:
        values["TopologyPrecision"] = pct(100 * float(pairs.get("precision", 0)))
        values["TopologyRecall"] = pct(100 * float(pairs.get("recall", 0)))

    # Native application and contention boundary.
    for suffix, macro_name in [("0", "MiniHotZeroTail"), ("5000", "MiniHotFiftyTail"), ("9900", "MiniHotNinetyNineTail")]:
        rows = read_csv(root / f"04-native/miniwarehouse-hot{suffix}/summary/summary.csv")
        if rows:
            workers = max_workers(rows)
            values[macro_name] = x(num(row_for(rows, "Rust-ACG", workers), "overlap_tail_x"))

    # Controlled contention: pre-execution critical path at the extremes.
    for lanes, macro_name in [(384, "Lane384PreexecCriticalMs"), (1, "LaneOnePreexecCriticalMs")]:
        rows = read_csv(root / f"06-contention/lanes-{lanes}/summary/summary.csv")
        if rows:
            workers = max_workers(rows)
            row = row_for(rows, "Rust-ACG", workers)
            values[macro_name] = ms(num(row, "pre_max_ms"), 0)

    # S3 exclusive phase decomposition.
    phase_records = []
    records_path = root / "03-s3-breakdown/records.jsonl"
    if records_path.is_file():
        with records_path.open(encoding="utf-8") as handle:
            phase_records = [json.loads(line) for line in handle if line.strip()]
        phase_records = [row for row in phase_records if row.get("strategy") == "cosmos-wasmd-symbgraph-rust"]
    if phase_records:
        workers = max(int(row.get("workers", 0)) for row in phase_records)
        phase_records = [row for row in phase_records if int(row.get("workers", 0)) == workers]
        totals = {
            key: sum(int(row.get(key, 0) or 0) for row in phase_records) / 1e6
            for key in [
                "symb_plan_nanos",
                "symb_preexecution_nanos",
                "symb_reconciliation_nanos",
                "symb_validation_nanos",
                "symb_replay_execution_nanos",
            ]
        }
        reconcile_other = max(
            0.0,
            totals["symb_reconciliation_nanos"]
            - totals["symb_validation_nanos"]
            - totals["symb_replay_execution_nanos"],
        )
        values["PhasePlanMs"] = ms(totals["symb_plan_nanos"], 1)
        values["PhasePreexecMs"] = ms(totals["symb_preexecution_nanos"], 1)
        values["PhaseValidateMs"] = ms(totals["symb_validation_nanos"], 1)
        values["PhaseReplayMs"] = ms(totals["symb_replay_execution_nanos"], 1)
        values["PhaseReconcileOtherMs"] = ms(reconcile_other, 1)

    # Block-size boundary at the largest controlled block.
    block_rows = read_csv(root / "10-block-size/tx-1024/summary/summary.csv")
    if block_rows:
        workers = max_workers(block_rows)
        values["Block1024TailSpeedup"] = x(num(row_for(block_rows, "Rust-ACG", workers), "overlap_tail_x"))

    # Prediction precision/recall at the three measured granularities.
    prediction = read_csv(root / "07-prediction/prediction-granularity/aggregate/summary-wide.csv")
    for granularity, prefix in [("fine", "Fine"), ("resource", "Resource"), ("profile", "Profile")]:
        row = next(
            (
                item
                for item in prediction
                if item.get("mode") == "probability-only"
                and item.get("param.contention") == "75pct"
                and item.get("param.operation_mix") == "full"
                and item.get("param.symbolic_granularity") == granularity
                and item.get("prediction_precision.mean") not in (None, "")
            ),
            None,
        )
        if row:
            values[f"Prediction{prefix}Precision"] = pct(100 * num(row, "prediction_precision.mean"), 1)
            values[f"Prediction{prefix}Recall"] = pct(100 * num(row, "prediction_recall.mean"), 1)

    # Adaptive recovery from hidden-key faults and workload regime changes.
    def recovery_value(path, filters, warmup):
        rows = read_csv(path)
        rows = [row for row in rows if row.get("metric") == "replayed_transactions" and row.get("mode") == "cost-aware"]
        for key, value in filters.items():
            rows = [row for row in rows if row.get("param." + key) == value]
        row = next((row for row in rows if int(num(row, "param.postchange_warmup_blocks")) == warmup), None)
        return num(row, "mean") if row else math.nan

    hidden_path = root / "07-prediction/prediction-recovery/aggregate/plot-long.csv"
    hidden_filters = {"contention": "75pct", "prediction_fault_mode": "hidden-key", "prediction_fault_rate_bps": "1000"}
    regime_path = root / "08-adaptation/aggregate/plot-long.csv"
    regime_filters = {"contention": "90pct", "warmup_hot_account_probability_bps": "1000", "acg.serial_bypass_enabled": "true"}
    for name, path, filters in [("Hidden", hidden_path, hidden_filters), ("Regime", regime_path, regime_filters)]:
        initial = recovery_value(path, filters, 0)
        after_one = recovery_value(path, filters, 1)
        if math.isfinite(initial):
            values[f"{name}ReplayInitial"] = f"{initial:.0f}"
        if math.isfinite(after_one):
            values[f"{name}ReplayAfterOne"] = f"{after_one:.0f}"

    # Ordering-window sensitivity at representative points.
    window = read_csv(root / "14-consensus-window-sensitivity/summary/consensus-sweep.csv")
    if window:
        workers = max_workers(window)
        for cutoff, suffix in [(0, "Zero"), (100, "OneHundred"), (300, "ThreeHundred"), (1000, "OneThousand")]:
            row = next(
                (
                    item
                    for item in window
                    if item.get("label") == "Rust-ACG"
                    and int(num(item, "workers")) == workers
                    and int(num(item, "consensus_window_ms")) == cutoff
                ),
                None,
            )
            if row:
                values[f"Window{suffix}CommitSpeedup"] = x(num(row, "commit_x"), 3)

    # Translation fidelity diagnostics.  Topology is machine-independent for a
    # frozen workload; execution-cost correlation is re-emitted for each run.
    critical = topology.get("critical_chain_fidelity", {}) if isinstance(topology, dict) else {}
    hot_chain = topology.get("vegeta_hot_key_chain_fidelity", {}) if isinstance(topology, dict) else {}
    if critical:
        values["CriticalPathRelativeError"] = pct(100 * float(critical.get("relative_error", 0)))
    if hot_chain:
        values["HotKeyRelativeError"] = pct(100 * float(hot_chain.get("relative_error", 0)))
    topology_text = root / "16-translation-fidelity/topology/native-topology-fidelity.txt"
    if topology_text.is_file():
        text = topology_text.read_text(encoding="utf-8")
        if "CriticalPathRelativeError" not in values:
            match = re.search(r"critical-path sum: source=\d+ native=\d+ ratio=[0-9.]+ relative_error=([0-9.]+)%", text)
            if match:
                values["CriticalPathRelativeError"] = pct(float(match.group(1)))
        if "HotKeyRelativeError" not in values:
            match = re.search(r"hot-key chain sum: source=\d+ native=\d+ ratio=[0-9.]+ relative_error=([0-9.]+)%", text)
            if match:
                values["HotKeyRelativeError"] = pct(float(match.group(1)))
    cost = read_json(root / "16-translation-fidelity/cost/summary.json")
    spearman = cost.get("correlation", {}).get("native_vs_steps_spearman") if isinstance(cost, dict) else None
    if spearman is not None:
        values["NativeStepsSpearman"] = f"{float(spearman):.2f}"

    semantics = read_json(root / "12-semantics/acceptance.json")
    accepted = semantics.get("accepted_runs") or semantics.get("runs")
    if accepted:
        values["SemanticAcceptedRuns"] = str(accepted)

    lines = [
        "% Auto-generated by evaluation/eurosys/export_latex_results.py.",
        "% Re-run evaluation/eurosys/postprocess.sh after a new campaign.",
    ]
    for name in sorted(values):
        lines.append(macro(name, values[name]))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(args.output)


if __name__ == "__main__":
    main()
