#!/usr/bin/env python3
"""Generate the consolidated EuroSys paper figures and selected supplements.

The plots deliberately separate replay-only speedup from overlap-aware Tail(C)
and modeled Commit(C).  They also avoid plotting missing metrics as zero and
prefer per-block medians across repeated samples for distribution plots.
"""
from __future__ import annotations

import argparse
import csv
import json
import statistics
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt

STRATEGIES = ["Serial", "BlockSTM", "AriaFB", "Vegeta", "Rust-ACG"]
NON_SERIAL = ["BlockSTM", "AriaFB", "Vegeta", "Rust-ACG"]
DISPLAY = {
    "Serial": "Serial",
    "BlockSTM": "BlockSTM",
    "AriaFB": "AriaFB",
    "Vegeta": "Vegeta",
    "Rust-ACG": "Ours",
    "ACG-Oracle": "Exact-access oracle",
}
LINE_STYLE = {
    "BlockSTM": dict(marker="o", linestyle="-"),
    "AriaFB": dict(marker="s", linestyle="--"),
    "Vegeta": dict(marker="^", linestyle="-."),
    "Rust-ACG": dict(marker="D", linestyle=":"),
    "Ideal": dict(linestyle="--"),
}
HATCH = {
    "BlockSTM": "",
    "AriaFB": "//",
    "Vegeta": "xx",
    "Rust-ACG": "..",
}


def read_csv(path: Path):
    if not Path(path).is_file():
        return []
    with Path(path).open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def num(row, key, default=0.0):
    try:
        value = row.get(key)
        return float(value) if value not in (None, "") else default
    except (TypeError, ValueError):
        return default


def present(row, key):
    return row.get(key) not in (None, "")


def max_workers(rows):
    return max((int(num(row, "workers")) for row in rows), default=0)


def selected(rows, label, workers=None):
    return [
        row
        for row in rows
        if row.get("label") == label
        and (workers is None or int(num(row, "workers")) == workers)
    ]


def style_for(label):
    return LINE_STYLE.get(label, dict(marker="o", linestyle="-"))


def display(label):
    return DISPLAY.get(label, label)


def plot_line(ax, rows, x_key, y_key, label, *, sort=True):
    rows = [row for row in rows if present(row, x_key) and present(row, y_key)]
    if sort:
        rows = sorted(rows, key=lambda row: num(row, x_key))
    if not rows:
        return []
    y = [num(row, y_key) for row in rows]
    err_key = y_key + "_ci95"
    err = [num(row, err_key) for row in rows]
    kwargs = style_for(label)
    ax.errorbar(
        [num(row, x_key) for row in rows],
        y,
        yerr=err if any(err) else None,
        capsize=2,
        label=display(label),
        **kwargs,
    )
    return y


def metric_long(path, metric, filters):
    rows = [row for row in read_csv(path) if row.get("metric") == metric]
    for key, value in filters.items():
        column = key if key.startswith("param.") else "param." + key
        rows = [row for row in rows if str(row.get(column, "")) == str(value)]
    return rows


def shared_legend(fig, axes, *, ncol=5):
    handles = []
    labels = []
    for ax in list(axes.flat) if hasattr(axes, "flat") else axes:
        h, l = ax.get_legend_handles_labels()
        for handle, label in zip(h, l):
            if label and label not in labels:
                handles.append(handle)
                labels.append(label)
        legend = ax.get_legend()
        if legend is not None:
            legend.remove()
    if handles:
        fig.legend(
            handles,
            labels,
            loc="upper center",
            bbox_to_anchor=(0.5, 0.995),
            ncol=min(ncol, len(labels)),
            frameon=False,
        )


def validation_note(*row_groups):
    samples = []
    for rows in row_groups:
        for row in rows:
            if present(row, "samples"):
                samples.append(int(num(row, "samples")))
    if samples and max(samples) <= 1:
        return "Local validation campaign: n=1 per configuration; confidence intervals omitted."
    return None


def save(fig, path, *, note=None, legend_axes=None, legend_ncol=5):
    path.parent.mkdir(parents=True, exist_ok=True)
    if legend_axes is not None:
        shared_legend(fig, legend_axes, ncol=legend_ncol)
    bottom = 0.045 if note else 0.02
    top = 0.91 if legend_axes is not None else 0.98
    if note:
        fig.text(0.995, 0.008, note, ha="right", va="bottom", fontsize=8)
    fig.tight_layout(rect=(0.0, bottom, 1.0, top))
    fig.savefig(path, bbox_inches="tight")
    plt.close(fig)


def grouped_bars(ax, datasets, labels, metric):
    width = 0.36
    centers = list(range(len(datasets)))
    for offset_index, label in enumerate(labels):
        offset = (offset_index - (len(labels) - 1) / 2) * width
        values = []
        errors = []
        for _, rows in datasets:
            workers = max_workers(rows)
            row = next(
                (
                    item
                    for item in rows
                    if item.get("label") == label
                    and int(num(item, "workers")) == workers
                ),
                None,
            )
            values.append(num(row, metric) if row else float("nan"))
            errors.append(num(row, metric + "_ci95") if row else 0.0)
        bars = ax.bar(
            [center + offset for center in centers],
            values,
            width=width,
            yerr=errors if any(errors) else None,
            capsize=2,
            label=display(label),
            hatch=HATCH.get(label, ""),
        )
        for bar, value in zip(bars, values):
            if value == value:
                ax.annotate(
                    f"{value:.2f}x",
                    (bar.get_x() + bar.get_width() / 2, bar.get_height()),
                    xytext=(0, 3),
                    textcoords="offset points",
                    ha="center",
                    va="bottom",
                    fontsize=7,
                )
    ax.set_xticks(centers, [name for name, _ in datasets])


def block_tail_medians(block_rows, dataset, workers, label):
    by_block = defaultdict(list)
    for row in block_rows:
        if (
            row.get("dataset") == dataset
            and int(num(row, "workers")) == workers
            and row.get("label") == label
        ):
            identity = row.get("block_number") or row.get("ordinal")
            by_block[identity].append(num(row, "tail_ms"))
    return sorted(statistics.median(values) for values in by_block.values())


def load_upper_bound(root):
    path = root / "05-upper-bound/upper-bound-report.json"
    if not path.is_file():
        return []
    try:
        return json.loads(path.read_text(encoding="utf-8")).get("rows", [])
    except (OSError, json.JSONDecodeError):
        return []


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--result-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()
    root = args.result_root
    out = args.output_dir

    if args.strict:
        required = [
            root / "01-s1/summary/summary.csv",
            root / "02-s4/summary/summary.csv",
            root / "03-s3-breakdown/records.jsonl",
            root / "04-native/native-mix/summary/summary.csv",
            root / "05-upper-bound/upper-bound-report.json",
            root / "07-prediction/prediction-granularity/aggregate/summary-wide.csv",
            root / "08-adaptation/aggregate/plot-long.csv",
            root / "10-block-size",
            root / "11-consensus/aggregate/plot-long.csv",
            root / "14-consensus-window-sensitivity/summary/consensus-sweep.csv",
            root / "eurosys-summary/economics-summary.csv",
        ]
        missing = [str(path) for path in required if not path.exists()]
        if missing:
            raise SystemExit(
                "missing required EuroSys plot inputs:\n  " + "\n  ".join(missing)
            )

    s1 = read_csv(root / "01-s1/summary/summary.csv")
    s4 = read_csv(root / "02-s4/summary/summary.csv")
    economics = read_csv(root / "eurosys-summary/economics-summary.csv")
    block_metrics = read_csv(root / "eurosys-summary/block-metrics.csv")
    datasets = [("S1", s1), ("S4", s4)]

    # Figure 1: real-workload headline.  Replay speedup is kept separate from
    # Tail(C) and Commit(C); the bottom row includes Vegeta for context.
    fig, axes = plt.subplots(2, 2, figsize=(10.0, 6.6))
    for ax, (name, rows) in zip(axes[0], datasets):
        workers = max_workers(rows)
        plotted = [
            row
            for row in rows
            if int(num(row, "workers")) == workers
            and row.get("label") in NON_SERIAL
        ]
        bars = ax.bar(
            [display(row["label"]) for row in plotted],
            [num(row, "throughput_speedup") for row in plotted],
            yerr=[num(row, "throughput_speedup_ci95") for row in plotted]
            if any(num(row, "throughput_speedup_ci95") for row in plotted)
            else None,
            capsize=2,
        )
        for bar, row in zip(bars, plotted):
            bar.set_hatch(HATCH.get(row["label"], ""))
        ax.axhline(1, color="black", linewidth=0.7)
        ax.set_title(f"{name}: post-order replay ({workers} workers)")
        ax.set_ylabel("Speedup vs Serial")
        ax.tick_params(axis="x", rotation=15)

    for ax, metric, title in [
        (axes[1, 0], "overlap_tail_x", "Overlap-aware Tail(C), C=300 ms"),
        (axes[1, 1], "commit_x", "Modeled Commit(C), C=300 ms"),
    ]:
        grouped_bars(ax, datasets, ["Vegeta", "Rust-ACG"], metric)
        ax.axhline(1, color="black", linewidth=0.7)
        ax.set_ylabel("Speedup vs Serial")
        ax.set_title(title)
    save(
        fig,
        out / "fig01-real-workload-headline.pdf",
        note=validation_note(s1, s4),
        legend_axes=axes[1:],
        legend_ncol=2,
    )

    # Figure 2: worker-count tradeoff + block-tail distributions.  For final
    # multi-sample campaigns the CDF is over per-block medians across samples,
    # avoiding a sample==0 dependency and avoiding pseudoreplication.
    fig, axes = plt.subplots(2, 2, figsize=(10.0, 6.6))
    for ax, (name, rows) in zip(axes[0], datasets):
        all_values = []
        for label in NON_SERIAL:
            all_values += plot_line(
                ax, selected(rows, label), "workers", "overlap_tail_x", label
            )
        ax.axhline(1, color="black", linewidth=0.7)
        ax.set_yscale("log")
        ax.set_xlabel("Workers")
        ax.set_ylabel("Tail speedup vs Serial (log scale)")
        ax.set_title(f"{name}: worker-count tradeoff")

    for ax, (name, rows) in zip(axes[1], datasets):
        workers = max_workers(rows)
        for label in NON_SERIAL:
            values = block_tail_medians(block_metrics, name, workers, label)
            if values:
                cdf_style = dict(style_for(label))
                cdf_style.pop("marker", None)
                ax.plot(
                    values,
                    [(index + 1) / len(values) for index in range(len(values))],
                    label=display(label),
                    **cdf_style,
                )
        ax.set_xlabel("Per-block Tail(C) (ms)")
        ax.set_ylabel("CDF")
        ax.set_title(f"{name}: block-tail distribution ({workers} workers)")
    save(
        fig,
        out / "fig02-scalability-and-tail-distribution.pdf",
        note=validation_note(s1, s4),
        legend_axes=axes,
        legend_ncol=4,
    )

    # Figure 3: native generality, controlled contention, and machine ceiling.
    # Tail(C) is used where an ordering window is meaningful so high contention
    # can expose the point at which speculation ceases to pay off.
    fig, axes = plt.subplots(2, 2, figsize=(10.0, 6.6))
    native = root / "04-native"
    hot = []
    native_groups = []
    for directory in native.glob("miniwarehouse-hot*"):
        rows = read_csv(directory / "summary/summary.csv")
        native_groups.append(rows)
        workers = max_workers(rows)
        try:
            hotness = int(directory.name.replace("miniwarehouse-hot", "")) / 100
        except ValueError:
            continue
        for label in NON_SERIAL:
            row = next(
                (
                    item
                    for item in rows
                    if item.get("label") == label
                    and int(num(item, "workers")) == workers
                ),
                None,
            )
            if row:
                hot.append((hotness, label, num(row, "overlap_tail_x")))
    for label in NON_SERIAL:
        points = sorted((h, value) for h, lab, value in hot if lab == label)
        if points:
            axes[0, 0].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **style_for(label),
            )
    axes[0, 0].axhline(1, color="black", linewidth=0.7)
    axes[0, 0].set_xlabel("Hot-warehouse probability (%)")
    axes[0, 0].set_ylabel("Tail speedup vs Serial")
    axes[0, 0].set_title("MiniWarehouse: contention boundary")

    native_mix = read_csv(native / "native-mix/summary/summary.csv")
    native_groups.append(native_mix)
    for label in NON_SERIAL:
        plot_line(
            axes[0, 1],
            selected(native_mix, label),
            "workers",
            "overlap_tail_x",
            label,
        )
    axes[0, 1].axhline(1, color="black", linewidth=0.7)
    axes[0, 1].set_yscale("log")
    axes[0, 1].set_xlabel("Workers")
    axes[0, 1].set_ylabel("Tail speedup vs Serial (log scale)")
    axes[0, 1].set_title("Native CW20/CW721/AMM mix")

    contention = {label: [] for label in NON_SERIAL}
    contention_groups = []
    for directory in (root / "06-contention").glob("lanes-*"):
        rows = read_csv(directory / "summary/summary.csv")
        contention_groups.append(rows)
        workers = max_workers(rows)
        lanes = int(directory.name.split("-")[-1])
        for label in NON_SERIAL:
            row = next(
                (
                    item
                    for item in rows
                    if item.get("label") == label
                    and int(num(item, "workers")) == workers
                ),
                None,
            )
            if row:
                contention[label].append((lanes, num(row, "overlap_tail_x")))
    for label, points in contention.items():
        points = sorted(points, reverse=True)
        if points:
            axes[1, 0].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **style_for(label),
            )
    axes[1, 0].set_xscale("log", base=2)
    lane_ticks = sorted({x for points in contention.values() for x, _ in points}, reverse=True)
    if lane_ticks:
        axes[1, 0].set_xticks(lane_ticks, [str(value) for value in lane_ticks])
    axes[1, 0].set_yscale("log")
    axes[1, 0].invert_xaxis()
    axes[1, 0].axhline(1, color="black", linewidth=0.7)
    axes[1, 0].set_xlabel("Independent lanes (fewer = more contention)")
    axes[1, 0].set_ylabel("Tail speedup vs Serial (log scale)")
    axes[1, 0].set_title("ConflictLab: controlled contention")

    # Always consume the dedicated upper-bound report.  summary.csv does not
    # contain the scale-vs-1-worker fields and previously plotted them as zero.
    upper_bound = load_upper_bound(root)
    for label in NON_SERIAL:
        rows = [row for row in upper_bound if row.get("label") == label]
        y_key = "acg_preexec_scale_vs_1w" if label == "Rust-ACG" else "post_scale_vs_1w"
        plot_line(axes[1, 1], rows, "workers", y_key, label)
    max_worker = max_workers(upper_bound)
    if max_worker:
        axes[1, 1].plot(
            [1, max_worker],
            [1, max_worker],
            label="Ideal",
            **style_for("Ideal"),
        )
    axes[1, 1].set_xlabel("Workers")
    axes[1, 1].set_ylabel("Scaling vs own 1-worker run")
    axes[1, 1].set_title("Zero-conflict machine ceiling")
    save(
        fig,
        out / "fig03-generality-contention-ceiling.pdf",
        note=validation_note(*(native_groups + contention_groups)),
        legend_axes=axes,
        legend_ncol=5,
    )

    # Figure 4: implementation cost.  Reconciliation is inclusive in the raw
    # counters, so plot only the residual reconciliation overhead after
    # subtracting validation and replay to avoid double counting.
    fig, axes = plt.subplots(2, 2, figsize=(10.0, 6.6))
    records_path = root / "03-s3-breakdown/records.jsonl"
    s3_summary = read_csv(root / "03-s3-breakdown/summary/summary.csv")
    if records_path.is_file():
        records = [
            json.loads(line)
            for line in records_path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        ]
        records = [
            row
            for row in records
            if row.get("strategy") == "cosmos-wasmd-symbgraph-rust"
        ]
        workers = max((int(row["workers"]) for row in records), default=0)
        records = [row for row in records if int(row["workers"]) == workers]
        totals = {
            key: sum(int(row.get(key, 0) or 0) for row in records) / 1e6
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
        phase_names = ["Plan", "Pre-exec", "Validate", "Replay", "Reconcile\nother"]
        phase_values = [
            totals["symb_plan_nanos"],
            totals["symb_preexecution_nanos"],
            totals["symb_validation_nanos"],
            totals["symb_replay_execution_nanos"],
            reconcile_other,
        ]
        bars = axes[0, 0].bar(phase_names, phase_values)
        axes[0, 0].set_yscale("log")
        axes[0, 0].set_ylabel("Summed time (ms, log scale)")
        axes[0, 0].set_title(f"S3 phase cost ({workers} workers)")
        axes[0, 0].bar_label(bars, labels=[f"{value:.1f}" for value in phase_values], fontsize=7)

    local_cost = []
    for name, _ in datasets:
        rows = [
            row
            for row in economics
            if row.get("dataset") == name and row.get("label") == "Rust-ACG"
        ]
        workers = max_workers(rows)
        row = next(
            (item for item in rows if int(num(item, "workers")) == workers), None
        )
        if row:
            local_cost.append((name, row))
    bars = axes[0, 1].bar(
        [name for name, _ in local_cost],
        [num(row, "local_elapsed_vs_serial") for _, row in local_cost],
        yerr=[num(row, "local_elapsed_vs_serial_ci95") for _, row in local_cost]
        if any(num(row, "local_elapsed_vs_serial_ci95") for _, row in local_cost)
        else None,
        capsize=2,
    )
    axes[0, 1].bar_label(
        bars,
        labels=[f"{num(row, 'local_elapsed_vs_serial'):.2f}x" for _, row in local_cost],
        fontsize=8,
    )
    axes[0, 1].axhline(1, color="black", linewidth=0.7)
    axes[0, 1].set_ylabel("(P + R) elapsed / Serial R")
    axes[0, 1].set_title("Local elapsed-work amplification")

    rss = []
    for name, subdir in [("S1", "01-s1"), ("S4", "02-s4")]:
        rows = read_csv(root / subdir / "summary/resource-usage.csv")
        workers = max_workers(rows)
        for label in ["Serial", "Rust-ACG"]:
            strategy = {
                "Serial": "cosmos-wasmd-direct-serial",
                "Rust-ACG": "cosmos-wasmd-symbgraph-rust",
            }[label]
            row = next(
                (
                    item
                    for item in rows
                    if item.get("strategy") == strategy
                    and int(num(item, "workers")) == workers
                ),
                None,
            )
            if row:
                rss.append((f"{name}\n{display(label)}", num(row, "max_rss_kib") / 1024))
    if rss:
        axes[1, 0].bar([name for name, _ in rss], [value for _, value in rss])
        axes[1, 0].set_ylabel("Peak RSS (MiB)")
        axes[1, 0].set_title("Isolated-process memory")

    block_size = {label: [] for label in NON_SERIAL}
    block_size_groups = []
    for directory in (root / "10-block-size").glob("tx-*"):
        rows = read_csv(directory / "summary/summary.csv")
        block_size_groups.append(rows)
        workers = max_workers(rows)
        transactions = int(directory.name.split("-")[-1])
        for label in NON_SERIAL:
            row = next(
                (
                    item
                    for item in rows
                    if item.get("label") == label
                    and int(num(item, "workers")) == workers
                ),
                None,
            )
            if row:
                block_size[label].append((transactions, num(row, "overlap_tail_x")))
    for label, points in block_size.items():
        points = sorted(points)
        if points:
            axes[1, 1].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **style_for(label),
            )
    axes[1, 1].set_xscale("log", base=2)
    axes[1, 1].set_yscale("log")
    axes[1, 1].axhline(1, color="black", linewidth=0.7)
    axes[1, 1].set_xlabel("Transactions/block")
    axes[1, 1].set_ylabel("Tail speedup vs Serial (log scale)")
    axes[1, 1].set_title("Block-size sensitivity")
    save(
        fig,
        out / "fig04-cost-and-overheads.pdf",
        note=validation_note(s3_summary, *block_size_groups),
        legend_axes=[axes[1, 1]],
        legend_ncol=4,
    )

    # Figure 5: prediction quality, exact-access headroom, and adaptation.
    fig, axes = plt.subplots(2, 2, figsize=(10.0, 6.6))
    workers = max_workers(s3_summary)
    oracle_rows = [
        row
        for row in s3_summary
        if int(num(row, "workers")) == workers
        and row.get("label") in {"Rust-ACG", "ACG-Oracle"}
    ]
    order = ["ACG-Oracle", "Rust-ACG"]
    oracle_rows = sorted(
        oracle_rows, key=lambda row: order.index(row["label"]) if row["label"] in order else 99
    )
    bars = axes[0, 0].bar(
        [display(row["label"]) for row in oracle_rows],
        [num(row, "post_ms") for row in oracle_rows],
        yerr=[num(row, "post_ms_ci95") for row in oracle_rows]
        if any(num(row, "post_ms_ci95") for row in oracle_rows)
        else None,
        capsize=2,
    )
    axes[0, 0].bar_label(
        bars,
        labels=[f"{num(row, 'post_ms'):.0f} ms" for row in oracle_rows],
        fontsize=8,
    )
    axes[0, 0].set_ylabel("Residual post-order time (ms)")
    axes[0, 0].set_title("Exact-access prediction headroom")

    wide = read_csv(root / "07-prediction/prediction-granularity/aggregate/summary-wide.csv")
    pareto = []
    for row in wide:
        if (
            row.get("param.contention") == "75pct"
            and row.get("param.operation_mix") == "full"
            and present(row, "prediction_precision.mean")
        ):
            pareto.append(
                (
                    num(row, "prediction_precision.mean"),
                    num(row, "throughput_speedup.mean"),
                    row.get("param.symbolic_granularity", ""),
                )
            )
    for precision, speedup, granularity in pareto:
        axes[0, 1].scatter([precision], [speedup])
        axes[0, 1].annotate(
            granularity,
            (precision, speedup),
            xytext=(4, 4),
            textcoords="offset points",
            fontsize=8,
        )
    axes[0, 1].axhline(1, color="black", linewidth=0.7)
    axes[0, 1].set_xlabel("Measured conflict precision")
    axes[0, 1].set_ylabel("Throughput speedup vs Serial")
    axes[0, 1].set_title("Adaptive prediction precision/performance")

    recovery = metric_long(
        root / "07-prediction/prediction-recovery/aggregate/plot-long.csv",
        "replayed_transactions",
        {
            "contention": "75pct",
            "prediction_fault_mode": "hidden-key",
            "prediction_fault_rate_bps": "1000",
        },
    )
    recovery = [row for row in recovery if row.get("mode") == "cost-aware"]
    recovery = sorted(recovery, key=lambda row: num(row, "param.postchange_warmup_blocks"))
    if recovery:
        axes[1, 0].plot(
            [num(row, "param.postchange_warmup_blocks") for row in recovery],
            [num(row, "mean") for row in recovery],
            marker="o",
        )
    axes[1, 0].set_xlabel("Blocks after hidden-key fault")
    axes[1, 0].set_ylabel("Replayed transactions")
    axes[1, 0].set_title("Hidden-dependency recovery (cost-aware)")

    adaptation = metric_long(
        root / "08-adaptation/aggregate/plot-long.csv",
        "replayed_transactions",
        {
            "contention": "90pct",
            "warmup_hot_account_probability_bps": "1000",
            "acg.serial_bypass_enabled": "true",
        },
    )
    adaptation = [row for row in adaptation if row.get("mode") == "cost-aware"]
    adaptation = sorted(
        adaptation, key=lambda row: num(row, "param.postchange_warmup_blocks")
    )
    if adaptation:
        axes[1, 1].plot(
            [num(row, "param.postchange_warmup_blocks") for row in adaptation],
            [num(row, "mean") for row in adaptation],
            marker="o",
        )
    axes[1, 1].set_xlabel("Blocks after regime change")
    axes[1, 1].set_ylabel("Replayed transactions")
    axes[1, 1].set_title("Low-to-hot adaptation (cost-aware)")
    mixed_note = None
    if s3_summary and max(int(num(row, "samples")) for row in s3_summary) <= 1:
        mixed_note = "S3 oracle panel: n=1 local validation run; controlled prediction/adaptation experiments use their recorded repetitions."
    save(fig, out / "fig05-prediction-and-adaptation.pdf", note=mixed_note)

    # Figure 6: ordering-window sensitivity + representative candidate/final
    # divergence cases.  Filter to the cost-aware policy so each divergence has
    # one value per cutoff rather than overplotting policy variants.
    fig, axes = plt.subplots(1, 2, figsize=(10.0, 3.8))
    consensus = read_csv(
        root / "14-consensus-window-sensitivity/summary/consensus-sweep.csv"
    )
    workers = max_workers(consensus)
    for label in NON_SERIAL:
        plot_line(
            axes[0],
            selected(consensus, label, workers),
            "consensus_window_ms",
            "commit_x",
            label,
        )
    axes[0].axhline(1, color="black", linewidth=0.7)
    axes[0].set_xlabel("Ordering window C (ms)")
    axes[0].set_ylabel("Modeled Commit(C) speedup")
    axes[0].set_title("Ordering-window sensitivity")

    divergence = metric_long(
        root / "11-consensus/aggregate/plot-long.csv",
        "throughput_speedup",
        {
            "prediction_quality": "bucketed",
            "contention": "75pct",
            "complexity": "mixed",
        },
    )
    divergence = [row for row in divergence if row.get("mode") == "cost-aware"]
    wanted = ["identical", "reorder-20pct", "tail-20pct", "tail-reorder-10pct"]
    names = {
        "identical": "Identical",
        "reorder-20pct": "20% reorder",
        "tail-20pct": "20% tail change",
        "tail-reorder-10pct": "10% tail + reorder",
    }
    for index, case in enumerate(wanted):
        rows = sorted(
            [row for row in divergence if row.get("param.consensus_divergence") == case],
            key=lambda row: num(row, "param.consensus_cutoff_ms"),
        )
        if rows:
            style = list(LINE_STYLE.values())[index % len(LINE_STYLE)]
            axes[1].plot(
                [num(row, "param.consensus_cutoff_ms") for row in rows],
                [num(row, "mean") for row in rows],
                label=names[case],
                **style,
            )
    axes[1].axhline(1, color="black", linewidth=0.7)
    axes[1].set_xlabel("Pre-order cutoff (ms)")
    axes[1].set_ylabel("Throughput speedup")
    axes[1].set_title("Candidate/final divergence")
    axes[0].legend(fontsize=7, frameon=False)
    axes[1].legend(fontsize=7, frameon=False)
    save(fig, out / "fig06-consensus-robustness.pdf")

    # Supplementary plots.
    compute = []
    for dataset in ["s1", "s4"]:
        compute += read_csv(
            root / f"15-compute-sensitivity/{dataset}/compute-sensitivity.csv"
        )
    if compute:
        fig, axes = plt.subplots(1, 2, figsize=(10.0, 3.6))
        for ax, dataset in zip(axes, ["S1", "S4"]):
            rows = [row for row in compute if row.get("dataset") == dataset]
            for label in NON_SERIAL:
                chosen = sorted(
                    [row for row in rows if row.get("label") == label],
                    key=lambda row: num(row, "scale"),
                )
                if chosen:
                    ax.plot(
                        [num(row, "scale") for row in chosen],
                        [num(row, "tail_speedup") for row in chosen],
                        label=display(label),
                        **style_for(label),
                    )
            ax.axhline(1, color="black", linewidth=0.7)
            ax.set_xlabel("Compute scale")
            ax.set_ylabel("Tail speedup vs Serial")
            ax.set_title(dataset)
        save(
            fig,
            out / "supp-compute-sensitivity.pdf",
            legend_axes=axes,
            legend_ncol=4,
        )

    cold = read_csv(root / "eurosys-summary/cold-start.csv")
    if cold:
        fig, axes = plt.subplots(1, 2, figsize=(10.0, 3.6))
        for ax, dataset in zip(axes, ["S1", "S4"]):
            rows = [row for row in cold if row.get("dataset") == dataset]
            ax.plot(
                [num(row, "ordinal") for row in rows],
                [num(row, "tail_speedup_vs_serial") for row in rows],
            )
            ax.set_xlabel("Block ordinal")
            ax.set_ylabel("Rolling Tail(C) speedup")
            ax.set_title(f"{dataset}: cold-start/adaptation")
        save(fig, out / "supp-real-workload-cold-start.pdf")

    print(out)


if __name__ == "__main__":
    main()
