#!/usr/bin/env python3
from __future__ import annotations

import argparse
from pathlib import Path

import matplotlib.pyplot as plt

from common import f, read_csv, save


def line_with_ci(ax, rows, *, xkey: str, ykey: str, label: str):
    rs = sorted(rows, key=lambda r: f(r, xkey))
    if not rs:
        return
    xs = [f(r, xkey) for r in rs]
    ys = [f(r, ykey) for r in rs]
    es = [f(r, ykey + "_ci95") for r in rs]
    ax.errorbar(xs, ys, yerr=es if any(es) else None, marker="o", capsize=3, label=label)


def plot_dataset(result_root: Path, output_dir: Path, subdir: str, slug: str, title: str):
    summary_dir = result_root / subdir / "summary"
    sweep_path = summary_dir / "consensus-sweep.csv"
    if not sweep_path.exists():
        return
    rows = read_csv(sweep_path)
    acg = [r for r in rows if r.get("label") == "Rust-ACG"]
    workers = sorted({int(f(r, "workers")) for r in acg})

    # 1) Middle-ground metric: execution tail after consensus has hidden available prework.
    fig, ax = plt.subplots(figsize=(6.4, 4.0))
    for w in workers:
        line_with_ci(ax, [r for r in acg if int(f(r, "workers")) == w], xkey="consensus_window_ms", ykey="overlap_tail_x", label=f"ACG {w}w")
    ax.axhline(1.0, linewidth=0.8)
    ax.set_xlabel("External consensus window C (ms)")
    ax.set_ylabel("Overlap-aware execution tail speedup")
    ax.set_title(f"{title}: pre-consensus work hidden by consensus")
    ax.legend()
    save(fig, output_dir / f"fig01c-{slug}-overlap-tail.pdf")
    plt.close(fig)

    # 2) Full proposal-to-commit sensitivity versus Serial under the same C.
    fig, ax = plt.subplots(figsize=(6.4, 4.0))
    for w in workers:
        line_with_ci(ax, [r for r in acg if int(f(r, "workers")) == w], xkey="consensus_window_ms", ykey="commit_x", label=f"ACG {w}w")
    # Add the strongest conventional baseline at the largest measured worker count as context.
    if workers:
        mw = max(workers)
        for label in ("BlockSTM", "Vegeta"):
            line_with_ci(ax, [r for r in rows if r.get("label") == label and int(f(r, "workers")) == mw], xkey="consensus_window_ms", ykey="commit_x", label=f"{label} {mw}w")
    ax.axhline(1.0, linewidth=0.8)
    ax.set_xlabel("External consensus window C (ms)")
    ax.set_ylabel("Proposal-to-commit speedup vs Serial")
    ax.set_title(f"{title}: consensus-window-aware commit sensitivity")
    ax.legend()
    save(fig, output_dir / f"fig01d-{slug}-commit-window.pdf")
    plt.close(fig)

    # 3) Probability that ACG finishes all prework before the ordering decision.
    fig, ax = plt.subplots(figsize=(6.4, 4.0))
    for w in workers:
        line_with_ci(ax, [r for r in acg if int(f(r, "workers")) == w], xkey="consensus_window_ms", ykey="pre_coverage_pct", label=f"ACG {w}w")
    ax.set_xlabel("External consensus window C (ms)")
    ax.set_ylabel("Blocks with P ≤ C (%)")
    ax.set_ylim(0, 105)
    ax.set_title(f"{title}: pre-consensus completion coverage")
    ax.legend()
    save(fig, output_dir / f"fig01e-{slug}-precoverage.pdf")
    plt.close(fig)

    # 4) Direct ACG advantage over the best deployable baseline, paired per sample.
    adv_path = summary_dir / "consensus-acg-vs-best.csv"
    if adv_path.exists():
        adv = read_csv(adv_path)
        fig, ax = plt.subplots(figsize=(6.4, 4.0))
        for w in sorted({int(f(r, "workers")) for r in adv}):
            line_with_ci(ax, [r for r in adv if int(f(r, "workers")) == w], xkey="consensus_window_ms", ykey="acg_vs_best_commit_x", label=f"ACG {w}w")
        ax.axhline(1.0, linewidth=0.8)
        ax.set_xlabel("External consensus window C (ms)")
        ax.set_ylabel("Commit throughput / best baseline")
        ax.set_title(f"{title}: ACG break-even against best baseline")
        ax.legend()
        save(fig, output_dir / f"fig01f-{slug}-break-even.pdf")
        plt.close(fig)

    # 5) Worker count selected by proposal-to-commit throughput at each C.
    optimal_path = summary_dir / "consensus-optimal-workers.csv"
    if optimal_path.exists():
        opt = [r for r in read_csv(optimal_path) if r.get("label") == "Rust-ACG"]
        if opt:
            opt = sorted(opt, key=lambda r: f(r, "consensus_window_ms"))
            fig, ax = plt.subplots(figsize=(6.4, 3.6))
            ax.step([f(r, "consensus_window_ms") for r in opt], [f(r, "optimal_workers") for r in opt], where="post")
            ax.set_xlabel("External consensus window C (ms)")
            ax.set_ylabel("Best measured ACG worker count")
            ax.set_title(f"{title}: worker count selected by commit latency")
            save(fig, output_dir / f"fig01g-{slug}-optimal-workers.pdf")
            plt.close(fig)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--result-root", type=Path, required=True)
    p.add_argument("--output-dir", type=Path, required=True)
    args = p.parse_args()
    plot_dataset(
        args.result_root, args.output_dir,
        "14-consensus-window-sensitivity", "s1",
        "S1-derived Wasmd consensus-window sensitivity",
    )


if __name__ == "__main__":
    main()
