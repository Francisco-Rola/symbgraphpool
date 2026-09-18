#!/usr/bin/env python3
"""Generate publication-facing EuroSys figures.

The plotting layer intentionally uses a compact, minimalist visual language:
normalized latency for the headline, sparse axes, direct annotations, shared
legends, and consistent marker/line encodings that remain distinguishable in
grayscale.  Figure 1 is a double-column headline; the remaining main and
supplementary figures are single-column, vertically stacked panels sized for
ACM two-column proceedings.  The script consumes only frozen experiment
artifacts and never mutates raw measurements.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path

import matplotlib.pyplot as plt
from matplotlib.lines import Line2D

STRATEGIES = ["BlockSTM", "AriaFB", "Vegeta", "Rust-ACG"]
DISPLAY = {
    "Serial": "Serial",
    "BlockSTM": "BlockSTM",
    "AriaFB": "AriaFB",
    "Vegeta": "Vegeta",
    "Rust-ACG": "Ours",
    "ACG-Oracle": "Exact-access oracle",
}

# Color-blind-safe and intentionally restrained.  Marker/line encodings are
# redundant with color so the figures remain interpretable in grayscale.
COLORS = {
    "Serial": "#111827",
    "BlockSTM": "#6B7280",
    "AriaFB": "#7C3AED",
    "Vegeta": "#D97706",
    "Rust-ACG": "#0F766E",
    "ACG-Oracle": "#2563EB",
    "Ideal": "#9CA3AF",
}
MARKERS = {
    "BlockSTM": "o",
    "AriaFB": "s",
    "Vegeta": "^",
    "Rust-ACG": "D",
    "ACG-Oracle": "P",
}
PAPER_COLUMN_WIDTH_IN = 3.33
STACKED_PANEL_HEIGHT_IN = 1.48

LINESTYLES = {
    "BlockSTM": "-",
    "AriaFB": "--",
    "Vegeta": "-.",
    "Rust-ACG": "-",
    "ACG-Oracle": ":",
    "Ideal": "--",
}


def apply_style():
    plt.rcParams.update(
        {
            "font.family": "DejaVu Sans",
            "font.size": 7.4,
            "axes.titlesize": 8.2,
            "axes.labelsize": 7.4,
            "xtick.labelsize": 6.9,
            "ytick.labelsize": 6.9,
            "legend.fontsize": 6.9,
            "axes.linewidth": 0.65,
            "lines.linewidth": 1.35,
            "lines.markersize": 4.2,
            "grid.linewidth": 0.5,
            "grid.alpha": 0.35,
            "pdf.fonttype": 42,
            "ps.fonttype": 42,
            "savefig.dpi": 300,
            "figure.facecolor": "white",
            "axes.facecolor": "white",
        }
    )


def read_csv(path: Path):
    if not Path(path).is_file():
        return []
    with Path(path).open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def read_json(path: Path):
    if not Path(path).is_file():
        return {}
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def read_jsonl(path: Path):
    if not Path(path).is_file():
        return []
    rows = []
    with Path(path).open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                rows.append(json.loads(line))
    return rows


def num(row, key, default=0.0):
    if row is None:
        return default
    try:
        value = row.get(key)
        return float(value) if value not in (None, "") else default
    except (TypeError, ValueError):
        return default


def present(row, key):
    return row is not None and row.get(key) not in (None, "")


def max_workers(rows):
    return max((int(num(row, "workers")) for row in rows), default=0)


def selected(rows, label, workers=None):
    return [
        row
        for row in rows
        if row.get("label") == label
        and (workers is None or int(num(row, "workers")) == workers)
    ]


def row_for(rows, label, workers):
    return next(
        (
            row
            for row in rows
            if row.get("label") == label and int(num(row, "workers")) == workers
        ),
        None,
    )


def display(label):
    return DISPLAY.get(label, label)


def color(label):
    return COLORS.get(label, "#4B5563")


def line_kwargs(label, *, emphasize=False):
    return {
        "color": color(label),
        "marker": MARKERS.get(label, "o"),
        "linestyle": LINESTYLES.get(label, "-"),
        "linewidth": 2.0 if emphasize or label == "Rust-ACG" else 1.25,
        "markersize": 4.7 if emphasize or label == "Rust-ACG" else 3.8,
        "markeredgewidth": 0.6,
        "zorder": 5 if label == "Rust-ACG" else 3,
    }


def clean_axis(ax, *, grid="y"):
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.spines["left"].set_color("#9CA3AF")
    ax.spines["bottom"].set_color("#9CA3AF")
    ax.tick_params(length=2.5, width=0.6, color="#6B7280")
    if grid == "x":
        ax.grid(axis="x", color="#D1D5DB")
    elif grid == "y":
        ax.grid(axis="y", color="#D1D5DB")
    elif grid == "both":
        ax.grid(color="#D1D5DB")
    ax.set_axisbelow(True)


def panel_title(ax, letter, title):
    ax.set_title(f"({letter})  {title}", loc="left", pad=5, fontweight="semibold")


def strategy_legend(fig, *, labels=None, y=1.01, ncol=4):
    labels = labels or STRATEGIES
    handles = [
        Line2D(
            [0],
            [0],
            color=color(label),
            marker=MARKERS.get(label, "o"),
            linestyle=LINESTYLES.get(label, "-"),
            linewidth=2.0 if label == "Rust-ACG" else 1.2,
            markersize=4.5,
            label=display(label),
        )
        for label in labels
    ]
    fig.legend(
        handles=handles,
        labels=[display(label) for label in labels],
        loc="upper center",
        bbox_to_anchor=(0.5, y),
        frameon=False,
        ncol=ncol,
        handlelength=2.1,
        columnspacing=1.1,
    )


def stacked_figure(panel_count, *, shared_legend=False):
    """Create a single-column ACM figure with vertically stacked panels."""
    legend_height = 0.44 if shared_legend else 0.0
    fig, axes = plt.subplots(
        panel_count,
        1,
        figsize=(PAPER_COLUMN_WIDTH_IN, panel_count * STACKED_PANEL_HEIGHT_IN + legend_height),
        squeeze=False,
    )
    return fig, list(axes[:, 0])


def save(fig, path, *, top=0.88, bottom=0.18):
    path.parent.mkdir(parents=True, exist_ok=True)
    fig.align_ylabels()
    fig.tight_layout(rect=(0.0, bottom, 1.0, top), h_pad=0.55)
    fig.savefig(path, bbox_inches="tight", pad_inches=0.025)
    plt.close(fig)


def plot_summary_line(ax, rows, y_key, label):
    rows = sorted(
        [row for row in rows if present(row, "workers") and present(row, y_key)],
        key=lambda row: num(row, "workers"),
    )
    if not rows:
        return
    ax.plot(
        [num(row, "workers") for row in rows],
        [num(row, y_key) for row in rows],
        label=display(label),
        **line_kwargs(label),
    )


def metric_long(path, metric, filters):
    rows = [row for row in read_csv(path) if row.get("metric") == metric]
    for key, value in filters.items():
        column = key if key.startswith("param.") else "param." + key
        rows = [row for row in rows if str(row.get(column, "")) == str(value)]
    return rows


def normalized_latency(speedup):
    return 100.0 / speedup if speedup and speedup > 0 else math.nan


def per_sample_normalized(summary_dir: Path, label, workers, speedup_key):
    rows = read_csv(summary_dir / "per-sample.csv")
    values = [
        normalized_latency(num(row, speedup_key))
        for row in rows
        if row.get("label") == label and int(num(row, "workers")) == workers
    ]
    return [value for value in values if math.isfinite(value)]


def draw_normalized_latency_panel(ax, dataset_specs, metric_key, title, letter):
    offsets = {
        "BlockSTM": -0.24,
        "AriaFB": -0.08,
        "Vegeta": 0.08,
        "Rust-ACG": 0.24,
    }
    bases = {"S1": 1.0, "S4": 0.0}
    for dataset, rows, summary_dir in dataset_specs:
        workers = max_workers(rows)
        for label in STRATEGIES:
            row = row_for(rows, label, workers)
            if not row:
                continue
            value = normalized_latency(num(row, metric_key))
            y = bases[dataset] + offsets[label]
            samples = per_sample_normalized(summary_dir, label, workers, metric_key)
            if len(samples) >= 2:
                ax.hlines(
                    y,
                    min(samples),
                    max(samples),
                    color=color(label),
                    linewidth=1.0,
                    alpha=0.45,
                    zorder=2,
                )
            ax.scatter(
                [value],
                [y],
                s=27 if label == "Rust-ACG" else 20,
                color=color(label),
                marker=MARKERS[label],
                linewidths=0.5,
                edgecolors="white",
                zorder=5 if label == "Rust-ACG" else 3,
            )
            if label in {"Vegeta", "Rust-ACG"}:
                ax.annotate(
                    f"{value:.0f}%" if value >= 10 else f"{value:.1f}%",
                    (value, y),
                    xytext=(3, 0),
                    textcoords="offset points",
                    va="center",
                    fontsize=6.4,
                    color=color(label),
                    fontweight="semibold" if label == "Rust-ACG" else "normal",
                )
    ax.axvline(100, color="#9CA3AF", linestyle="--", linewidth=0.8)
    ax.set_xlim(0, 132)
    ax.set_yticks([1, 0], ["S1", "S4"])
    ax.set_xlabel("Latency (% of Serial; lower is better)")
    panel_title(ax, letter, title)
    clean_axis(ax, grid="x")


def per_block_normalized_tail(block_rows, dataset, workers, label):
    # Compute the ratio within each (sample, block) first, then take a median
    # across repeated samples for each block.  This avoids pseudoreplication.
    serial = {}
    target = {}
    for row in block_rows:
        if row.get("dataset") != dataset or int(num(row, "workers")) != workers:
            continue
        key = (row.get("sample"), row.get("block_number") or row.get("ordinal"))
        if row.get("label") == "Serial":
            serial[key] = num(row, "tail_ms")
        elif row.get("label") == label:
            target[key] = num(row, "tail_ms")
    by_block = defaultdict(list)
    for key, value in target.items():
        denom = serial.get(key)
        if denom and denom > 0:
            by_block[key[1]].append(value / denom)
    return sorted(statistics.median(values) for values in by_block.values() if values)


def load_upper_bound(root):
    data = read_json(root / "05-upper-bound/upper-bound-report.json")
    return data.get("rows", []) if isinstance(data, dict) else []


def resource_overhead_pct(root, subdir, workers):
    rows = read_csv(root / subdir / "summary/resource-usage.csv")
    serial = next(
        (
            num(row, "max_rss_kib")
            for row in rows
            if row.get("strategy") == "cosmos-wasmd-direct-serial"
            and int(num(row, "workers")) == workers
        ),
        None,
    )
    ours = next(
        (
            num(row, "max_rss_kib")
            for row in rows
            if row.get("strategy") == "cosmos-wasmd-symbgraph-rust"
            and int(num(row, "workers")) == workers
        ),
        None,
    )
    if not serial or ours is None:
        return math.nan
    return 100.0 * (ours / serial - 1.0)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--result-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()
    root = args.result_root
    out = args.output_dir
    apply_style()

    if args.strict:
        required = [
            root / "01-s1/summary/summary.csv",
            root / "02-s4/summary/summary.csv",
            root / "03-s3-breakdown/records.jsonl",
            root / "04-native/native-mix/summary/summary.csv",
            root / "05-upper-bound/upper-bound-report.json",
            root / "07-prediction/prediction-granularity/aggregate/summary-wide.csv",
            root / "08-adaptation/aggregate/plot-long.csv",
            root / "11-consensus/aggregate/plot-long.csv",
            root / "14-consensus-window-sensitivity/summary/consensus-sweep.csv",
            root / "eurosys-summary/economics-summary.csv",
        ]
        missing = [str(path) for path in required if not path.exists()]
        if missing:
            raise SystemExit("missing required plot inputs:\n  " + "\n  ".join(missing))

    s1_dir = root / "01-s1/summary"
    s4_dir = root / "02-s4/summary"
    s1 = read_csv(s1_dir / "summary.csv")
    s4 = read_csv(s4_dir / "summary.csv")
    economics = read_csv(root / "eurosys-summary/economics-summary.csv")
    block_metrics = read_csv(root / "eurosys-summary/block-metrics.csv")
    datasets = [("S1", s1, s1_dir), ("S4", s4, s4_dir)]

    # ------------------------------------------------------------------
    # Figure 1.  One coherent visual unit: all three panels show normalized
    # latency relative to Serial.  This avoids juxtaposing 23x and 1.15x bars
    # on unrelated scales and makes the architectural effect immediately clear.
    # ------------------------------------------------------------------
    fig, axes = plt.subplots(1, 3, figsize=(7.15, 2.15), sharex=True)
    draw_normalized_latency_panel(
        axes[0], datasets, "throughput_speedup", "Post-order execution", "a"
    )
    draw_normalized_latency_panel(
        axes[1], datasets, "overlap_tail_x", "Tail($C$), $C$=300 ms", "b"
    )
    draw_normalized_latency_panel(
        axes[2], datasets, "commit_x", "Modeled commit, $C$=300 ms", "c"
    )
    axes[1].set_yticklabels([])
    axes[2].set_yticklabels([])
    strategy_legend(fig, y=1.02, ncol=4)
    save(fig, out / "fig01-real-workload-headline.pdf", top=0.84, bottom=0.24)

    # ------------------------------------------------------------------
    # Figure 2. Worker-count tradeoff and the distribution of per-block tail
    # latency, normalized by the matching Serial block.
    # ------------------------------------------------------------------
    fig, axes = stacked_figure(3, shared_legend=True)
    for index, (ax, (name, rows, _)) in enumerate(zip(axes[:2], datasets)):
        for label in STRATEGIES:
            plot_summary_line(ax, selected(rows, label), "overlap_tail_x", label)
        ax.axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
        ax.set_yscale("log")
        ax.set_xlabel("Workers")
        if index == 0:
            ax.set_ylabel("Tail($C$) speedup vs Serial")
        panel_title(ax, chr(ord("a") + index), f"{name}: worker-count tradeoff")
        clean_axis(ax, grid="y")

    ax = axes[2]
    for dataset, rows, _ in datasets:
        workers = max_workers(rows)
        for label in ["Vegeta", "Rust-ACG"]:
            values = per_block_normalized_tail(block_metrics, dataset, workers, label)
            if not values:
                continue
            line_style = "-" if dataset == "S1" else "--"
            ax.plot(
                [100.0 * value for value in values],
                [(index + 1) / len(values) for index in range(len(values))],
                color=color(label),
                linestyle=line_style,
                linewidth=1.8 if label == "Rust-ACG" else 1.2,
                label=f"{display(label)} / {dataset}",
            )
    ax.set_xscale("log")
    ax.set_xlabel("Per-block Tail($C$) (% of Serial)")
    ax.set_ylabel("CDF")
    panel_title(ax, "c", "Max-worker block distribution")
    clean_axis(ax, grid="x")
    ax.legend(frameon=False, fontsize=6.1, loc="lower right", ncol=2)
    strategy_legend(fig, y=0.995, ncol=2)
    save(fig, out / "fig02-scalability-and-tail-distribution.pdf", top=0.91, bottom=0.07)

    # ------------------------------------------------------------------
    # Figure 3. Controlled contention and scaling ceiling.  NativeMix is kept
    # in the text because its single max-worker point is more legible there than
    # as a fourth small panel.
    # ------------------------------------------------------------------
    fig, axes = stacked_figure(3, shared_legend=True)
    native = root / "04-native"
    hot = {label: [] for label in STRATEGIES}
    for directory in native.glob("miniwarehouse-hot*"):
        rows = read_csv(directory / "summary/summary.csv")
        workers = max_workers(rows)
        try:
            hotness = int(directory.name.replace("miniwarehouse-hot", "")) / 100.0
        except ValueError:
            continue
        for label in STRATEGIES:
            row = row_for(rows, label, workers)
            if row:
                hot[label].append((hotness, num(row, "overlap_tail_x")))
    for label in STRATEGIES:
        points = sorted(hot[label])
        if points:
            axes[0].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **line_kwargs(label),
            )
    axes[0].axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
    axes[0].set_xlabel("Hot warehouse (%)")
    axes[0].set_ylabel("Tail($C$) speedup")
    panel_title(axes[0], "a", "Application contention")
    clean_axis(axes[0], grid="y")

    contention = {label: [] for label in STRATEGIES}
    lane_ticks = set()
    for directory in (root / "06-contention").glob("lanes-*"):
        rows = read_csv(directory / "summary/summary.csv")
        workers = max_workers(rows)
        lanes = int(directory.name.split("-")[-1])
        lane_ticks.add(lanes)
        for label in STRATEGIES:
            row = row_for(rows, label, workers)
            if row:
                contention[label].append((lanes, num(row, "commit_x")))
    for label in STRATEGIES:
        points = sorted(contention[label], reverse=True)
        if points:
            axes[1].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **line_kwargs(label),
            )
    lane_ticks = sorted(lane_ticks, reverse=True)
    axes[1].set_xscale("log", base=2)
    if lane_ticks:
        axes[1].set_xticks(lane_ticks, [str(value) for value in lane_ticks])
    axes[1].invert_xaxis()
    axes[1].axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
    axes[1].set_xlabel("Independent lanes (fewer = hotter)")
    axes[1].set_ylabel("Modeled commit speedup")
    panel_title(axes[1], "b", "Controlled contention")
    clean_axis(axes[1], grid="y")

    upper_bound = load_upper_bound(root)
    for label in STRATEGIES:
        rows = [row for row in upper_bound if row.get("label") == label]
        key = "acg_preexec_scale_vs_1w" if label == "Rust-ACG" else "post_scale_vs_1w"
        rows = sorted(rows, key=lambda row: num(row, "workers"))
        if rows:
            axes[2].plot(
                [num(row, "workers") for row in rows],
                [num(row, key) for row in rows],
                label=display(label),
                **line_kwargs(label),
            )
    max_worker = max_workers(upper_bound)
    if max_worker:
        axes[2].plot(
            [1, max_worker],
            [1, max_worker],
            color=color("Ideal"),
            linestyle="--",
            linewidth=0.9,
            label="Ideal",
        )
    axes[2].set_xlabel("Workers")
    axes[2].set_ylabel("Scaling vs 1 worker")
    panel_title(axes[2], "c", "Zero-conflict ceiling")
    clean_axis(axes[2], grid="y")
    strategy_legend(fig, y=0.995, ncol=2)
    save(fig, out / "fig03-generality-contention-ceiling.pdf", top=0.91, bottom=0.07)

    # ------------------------------------------------------------------
    # Figure 4. Cost decomposition, overhead, and block-size sensitivity.
    # ------------------------------------------------------------------
    fig, axes = stacked_figure(3)
    records = [
        row
        for row in read_jsonl(root / "03-s3-breakdown/records.jsonl")
        if row.get("strategy") == "cosmos-wasmd-symbgraph-rust"
    ]
    workers = max((int(row.get("workers", 0)) for row in records), default=0)
    records = [row for row in records if int(row.get("workers", 0)) == workers]
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
        totals.get("symb_reconciliation_nanos", 0.0)
        - totals.get("symb_validation_nanos", 0.0)
        - totals.get("symb_replay_execution_nanos", 0.0),
    )
    phase = [
        ("Pre-exec", totals.get("symb_preexecution_nanos", 0.0)),
        ("Plan", totals.get("symb_plan_nanos", 0.0)),
        ("Replay", totals.get("symb_replay_execution_nanos", 0.0)),
        ("Reconcile", reconcile_other),
        ("Validate", totals.get("symb_validation_nanos", 0.0)),
    ]
    phase = sorted(phase, key=lambda item: item[1])
    axes[0].hlines(
        range(len(phase)),
        [1e-3] * len(phase),
        [value for _, value in phase],
        color="#D1D5DB",
        linewidth=2.0,
    )
    axes[0].scatter(
        [value for _, value in phase],
        range(len(phase)),
        color=color("Rust-ACG"),
        s=25,
        zorder=4,
    )
    for y, (_, value) in enumerate(phase):
        axes[0].annotate(
            f"{value:.1f} ms",
            (value, y),
            xytext=(4, 0),
            textcoords="offset points",
            va="center",
            fontsize=6.2,
        )
    axes[0].set_yticks(range(len(phase)), [name for name, _ in phase])
    axes[0].set_xscale("log")
    axes[0].set_xlabel("Summed phase time (ms, log scale)")
    panel_title(axes[0], "a", "S3 implementation cost")
    clean_axis(axes[0], grid="x")

    overhead = {"S1": {}, "S4": {}}
    for dataset, subdir in [("S1", "01-s1"), ("S4", "02-s4")]:
        econ = [
            row
            for row in economics
            if row.get("dataset") == dataset and row.get("label") == "Rust-ACG"
        ]
        mw = max_workers(econ)
        row = row_for(econ, "Rust-ACG", mw)
        overhead[dataset]["Execution work"] = 100.0 * (num(row, "local_elapsed_vs_serial") - 1.0)
        overhead[dataset]["Peak RSS"] = resource_overhead_pct(root, subdir, mw)
    positions = {"S1": 0, "S4": 1}
    for metric, marker, shade in [
        ("Execution work", "D", color("Rust-ACG")),
        ("Peak RSS", "o", "#6B7280"),
    ]:
        values = [overhead[d][metric] for d in ["S1", "S4"]]
        axes[1].scatter(
            [positions[d] for d in ["S1", "S4"]],
            values,
            marker=marker,
            color=shade,
            s=28,
            label=metric,
            zorder=4,
        )
        for x, value in zip([0, 1], values):
            axes[1].annotate(
                f"{value:+.1f}%",
                (x, value),
                xytext=(0, 5 if value >= 0 else -10),
                textcoords="offset points",
                ha="center",
                fontsize=6.4,
            )
    axes[1].axhline(0, color="#9CA3AF", linewidth=0.8)
    axes[1].set_xticks([0, 1], ["S1", "S4"])
    axes[1].set_ylabel("Overhead vs Serial (%)")
    overhead_values = [
        value
        for dataset in ["S1", "S4"]
        for value in overhead[dataset].values()
        if math.isfinite(value)
    ]
    if overhead_values:
        axes[1].set_ylim(min(-5.0, min(overhead_values) - 2.5), max(12.0, max(overhead_values) + 5.0))
    panel_title(axes[1], "b", "Resource overhead")
    clean_axis(axes[1], grid="y")
    axes[1].legend(frameon=False, fontsize=6.3, loc="upper left")

    block_size = {label: [] for label in STRATEGIES}
    for directory in (root / "10-block-size").glob("tx-*"):
        rows = read_csv(directory / "summary/summary.csv")
        mw = max_workers(rows)
        tx = int(directory.name.split("-")[-1])
        for label in STRATEGIES:
            row = row_for(rows, label, mw)
            if row:
                block_size[label].append((tx, num(row, "overlap_tail_x")))
    for label in STRATEGIES:
        points = sorted(block_size[label])
        if points:
            axes[2].plot(
                [x for x, _ in points],
                [y for _, y in points],
                label=display(label),
                **line_kwargs(label),
            )
    axes[2].set_xscale("log", base=2)
    axes[2].set_yscale("log")
    axes[2].axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
    axes[2].set_xlabel("Transactions/block")
    axes[2].set_ylabel("Tail($C$) speedup")
    block_speedups = [value for points in block_size.values() for _, value in points if value > 0]
    if block_speedups:
        axes[2].set_ylim(0.9, max(block_speedups) * 1.7)
    panel_title(axes[2], "c", "Block-size sensitivity")
    clean_axis(axes[2], grid="y")
    axes[2].legend(frameon=False, fontsize=6.0, ncol=2, loc="upper left")
    save(fig, out / "fig04-cost-and-overheads.pdf", top=0.985, bottom=0.07)

    # ------------------------------------------------------------------
    # Figure 5. Prediction quality, oracle headroom, and recovery.
    # ------------------------------------------------------------------
    fig, axes = stacked_figure(3)
    wide = read_csv(root / "07-prediction/prediction-granularity/aggregate/summary-wide.csv")
    quality = []
    for row in wide:
        if (
            row.get("param.contention") == "75pct"
            and row.get("param.operation_mix") == "full"
            and row.get("mode") == "probability-only"
            and present(row, "prediction_precision.mean")
        ):
            quality.append(
                (
                    row.get("param.symbolic_granularity", ""),
                    num(row, "prediction_precision.mean"),
                    num(row, "prediction_recall.mean"),
                )
            )
    quality_order = {"fine": 0, "resource": 1, "profile": 2}
    quality = sorted(quality, key=lambda item: quality_order.get(item[0], 99))
    x = list(range(len(quality)))
    if quality:
        axes[0].plot(
            x,
            [100 * item[1] for item in quality],
            color=color("Rust-ACG"),
            marker="D",
            linewidth=1.7,
            label="Precision",
        )
        axes[0].plot(
            x,
            [100 * item[2] for item in quality],
            color="#6B7280",
            marker="o",
            linestyle="--",
            linewidth=1.2,
            label="Recall",
        )
        axes[0].set_xticks(x, [item[0].capitalize() for item in quality])
    axes[0].set_ylim(0, 105)
    axes[0].set_ylabel("Conflict prediction (%)")
    panel_title(axes[0], "a", "Symbolic granularity")
    clean_axis(axes[0], grid="y")
    axes[0].legend(frameon=False, fontsize=6.3, loc="lower left", ncol=2)

    s3_summary = read_csv(root / "03-s3-breakdown/summary/summary.csv")
    mw = max_workers(s3_summary)
    oracle = row_for(s3_summary, "ACG-Oracle", mw)
    ours = row_for(s3_summary, "Rust-ACG", mw)
    oracle_points = [
        ("Exact access", num(oracle, "post_ms"), "ACG-Oracle"),
        ("Predicted", num(ours, "post_ms"), "Rust-ACG"),
    ]
    y = [1, 0]
    axes[1].hlines(
        y,
        [0, 0],
        [value for _, value, _ in oracle_points],
        color="#D1D5DB",
        linewidth=2.2,
    )
    for ypos, (name, value, label) in zip(y, oracle_points):
        axes[1].scatter([value], [ypos], color=color(label), marker=MARKERS[label], s=30, zorder=4)
        axes[1].annotate(
            f"{value:.0f} ms",
            (value, ypos),
            xytext=(4, 0),
            textcoords="offset points",
            va="center",
            fontsize=6.5,
        )
    axes[1].set_yticks(y, [item[0] for item in oracle_points])
    axes[1].set_xlabel("Residual post-order time (ms)")
    panel_title(axes[1], "b", "Exact-access headroom")
    clean_axis(axes[1], grid="x")
    if ours and oracle:
        axes[1].text(
            0.98,
            0.08,
            f"Commit: {num(oracle, 'commit_x'):.2f}x vs {num(ours, 'commit_x'):.2f}x",
            transform=axes[1].transAxes,
            ha="right",
            va="bottom",
            fontsize=6.2,
            color="#4B5563",
        )

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
    for name, rows, shade, marker in [
        ("Hidden dependency", recovery, color("Rust-ACG"), "D"),
        ("Regime change", adaptation, color("Vegeta"), "o"),
    ]:
        rows = sorted(rows, key=lambda row: num(row, "param.postchange_warmup_blocks"))
        if rows:
            axes[2].plot(
                [num(row, "param.postchange_warmup_blocks") for row in rows],
                [num(row, "mean") for row in rows],
                color=shade,
                marker=marker,
                linewidth=1.5,
                label=name,
            )
    axes[2].set_xlabel("Blocks after change")
    axes[2].set_ylabel("Replayed transactions")
    recovery_values = [num(row, "mean") for row in recovery + adaptation]
    if recovery_values:
        axes[2].set_ylim(bottom=min(0.0, min(recovery_values)), top=max(recovery_values) * 1.25)
    panel_title(axes[2], "c", "Runtime recovery")
    clean_axis(axes[2], grid="y")
    axes[2].legend(frameon=False, fontsize=6.0, loc="upper right", ncol=1)
    save(fig, out / "fig05-prediction-and-adaptation.pdf", top=0.985, bottom=0.07)

    # ------------------------------------------------------------------
    # Figure 6. Robustness to the available ordering window and to candidate /
    # final-set divergence.
    # ------------------------------------------------------------------
    fig, axes = stacked_figure(2)
    consensus = read_csv(root / "14-consensus-window-sensitivity/summary/consensus-sweep.csv")
    mw = max_workers(consensus)
    for label in STRATEGIES:
        rows = sorted(selected(consensus, label, mw), key=lambda row: num(row, "consensus_window_ms"))
        if rows:
            axes[0].plot(
                [num(row, "consensus_window_ms") for row in rows],
                [num(row, "commit_x") for row in rows],
                label=display(label),
                **line_kwargs(label),
            )
    axes[0].axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
    axes[0].set_xlabel("Ordering window $C$ (ms)")
    axes[0].set_ylabel("Modeled commit speedup")
    panel_title(axes[0], "a", "Ordering-window sensitivity")
    clean_axis(axes[0], grid="y")
    axes[0].legend(frameon=False, fontsize=6.1, ncol=2, loc="best")

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
    wanted = [
        ("identical", "Identical", "#0F766E", "-"),
        ("reorder-20pct", "20% reorder", "#D97706", "--"),
        ("tail-20pct", "20% tail change", "#7C3AED", "-."),
        ("tail-reorder-10pct", "10% tail + reorder", "#6B7280", ":"),
    ]
    for case, name, shade, linestyle in wanted:
        rows = sorted(
            [row for row in divergence if row.get("param.consensus_divergence") == case],
            key=lambda row: num(row, "param.consensus_cutoff_ms"),
        )
        if rows:
            axes[1].plot(
                [num(row, "param.consensus_cutoff_ms") for row in rows],
                [num(row, "mean") for row in rows],
                color=shade,
                linestyle=linestyle,
                marker="o",
                markersize=3.6,
                linewidth=1.35,
                label=name,
            )
    axes[1].axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
    axes[1].set_xlabel("Pre-order cutoff (ms)")
    axes[1].set_ylabel("Throughput speedup")
    panel_title(axes[1], "b", "Candidate/final divergence")
    clean_axis(axes[1], grid="y")
    axes[1].legend(frameon=False, fontsize=6.1, ncol=2, loc="best")
    save(fig, out / "fig06-consensus-robustness.pdf", top=0.985, bottom=0.09)

    # Supplements use the same visual language.
    compute = []
    for dataset in ["s1", "s4"]:
        compute += read_csv(root / f"15-compute-sensitivity/{dataset}/compute-sensitivity.csv")
    if compute:
        fig, axes = stacked_figure(2, shared_legend=True)
        for index, (ax, dataset) in enumerate(zip(axes, ["S1", "S4"])):
            rows = [row for row in compute if row.get("dataset") == dataset]
            for label in STRATEGIES:
                chosen = sorted(
                    [row for row in rows if row.get("label") == label],
                    key=lambda row: num(row, "scale"),
                )
                if chosen:
                    ax.plot(
                        [num(row, "scale") for row in chosen],
                        [num(row, "tail_speedup") for row in chosen],
                        label=display(label),
                        **line_kwargs(label),
                    )
            ax.axhline(1, color="#9CA3AF", linestyle="--", linewidth=0.8)
            ax.set_xlabel("Compute scale")
            if index == 0:
                ax.set_ylabel("Tail($C$) speedup")
            panel_title(ax, chr(ord("a") + index), dataset)
            clean_axis(ax, grid="y")
        strategy_legend(fig, y=0.995, ncol=2)
        save(fig, out / "supp-compute-sensitivity.pdf", top=0.88, bottom=0.09)

    cold = read_csv(root / "eurosys-summary/cold-start.csv")
    if cold:
        fig, axes = stacked_figure(2)
        for index, (ax, dataset) in enumerate(zip(axes, ["S1", "S4"])):
            rows = [row for row in cold if row.get("dataset") == dataset]
            ax.plot(
                [num(row, "ordinal") for row in rows],
                [num(row, "tail_speedup_vs_serial") for row in rows],
                color=color("Rust-ACG"),
                linewidth=1.6,
            )
            ax.set_xlabel("Block ordinal")
            if index == 0:
                ax.set_ylabel("Rolling Tail($C$) speedup")
            panel_title(ax, chr(ord("a") + index), f"{dataset}: cold start")
            clean_axis(ax, grid="y")
        save(fig, out / "supp-real-workload-cold-start.pdf", top=0.985, bottom=0.09)

    print(out)


if __name__ == "__main__":
    main()
