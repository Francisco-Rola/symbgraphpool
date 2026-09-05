#!/usr/bin/env python3
"""Summarize the native Wasmd ConflictLab zero-conflict upper-bound experiment."""
from __future__ import annotations

import argparse
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any

ORDER = [
    "cosmos-wasmd-direct-serial",
    "cosmos-wasmd-block-stm",
    "cosmos-wasmd-aria-fb",
    "cosmos-wasmd-vegeta",
    "cosmos-wasmd-symbgraph-rust",
]
LABEL = {
    "cosmos-wasmd-direct-serial": "Serial",
    "cosmos-wasmd-block-stm": "BlockSTM",
    "cosmos-wasmd-aria-fb": "AriaFB",
    "cosmos-wasmd-vegeta": "Vegeta",
    "cosmos-wasmd-symbgraph-rust": "Rust-ACG",
}


def read_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open(encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{lineno}: invalid JSON: {exc}") from exc
    return rows


def ratio(num: float, den: float) -> float:
    return num / den if den else 0.0


def mean_ci95(values: list[float]) -> tuple[float, float]:
    if not values:
        return 0.0, 0.0
    mean = statistics.fmean(values)
    if len(values) < 2:
        return mean, 0.0
    return mean, 1.96 * statistics.stdev(values) / math.sqrt(len(values))


def sample_metrics(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    groups: dict[tuple[str, int, int], list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        strategy = str(row.get("strategy", ""))
        if strategy in LABEL:
            groups[(strategy, int(row["workers"]), int(row["sample"]))].append(row)

    out: list[dict[str, Any]] = []
    for (strategy, workers, sample), rs in sorted(groups.items()):
        tx = sum(int(r.get("transactions", 0) or 0) for r in rs)
        post_ns = sum(int(r.get("post_consensus_nanos", 0) or 0) for r in rs)
        pre_ns = sum(int(r.get("pre_consensus_nanos", 0) or 0) for r in rs)
        reexec = sum(int(r.get("reexecutions", 0) or 0) for r in rs)
        attempts = sum(int(r.get("execution_attempts", 0) or 0) for r in rs)
        discovered = sum(int(r.get("discovered_conflicts", 0) or 0) for r in rs)
        forward = sum(int(r.get("forward_fallbacks", 0) or 0) for r in rs)
        safety = sum(int(r.get("safety_replays", 0) or 0) for r in rs)
        acg_deps = sum(int(r.get("symb_dependency_edges", 0) or 0) for r in rs)
        acg_oracle_edges = sum(int(r.get("symb_oracle_conflict_edges", 0) or 0) for r in rs)
        acg_pre_ns = sum(int(r.get("symb_preexecution_nanos", 0) or 0) for r in rs)
        acg_plan_ns = sum(int(r.get("symb_plan_nanos", 0) or 0) for r in rs)
        acg_util = statistics.fmean(float(r.get("symb_worker_utilization", 0.0) or 0.0) for r in rs)
        acg_max_active = max((int(r.get("symb_max_active", 0) or 0) for r in rs), default=0)
        vegeta_work = sum(int(r.get("vegeta_post_exec_work_nanos", 0) or 0) for r in rs)
        vegeta_span = sum(int(r.get("vegeta_post_exec_span_nanos", 0) or 0) for r in rs)
        vegeta_ready_cost = sum(int(r.get("vegeta_ready_worker_lower_bound_cost", 0) or 0) for r in rs)
        vegeta_total_cost = sum(int(r.get("vegeta_total_estimated_cost", 0) or 0) for r in rs)
        aria_work = sum(int(r.get("aria_initial_exec_work_nanos", 0) or 0) for r in rs)
        aria_wall = sum(int(r.get("aria_initial_batch_nanos", 0) or 0) for r in rs)
        out.append(
            {
                "strategy": strategy,
                "label": LABEL[strategy],
                "workers": workers,
                "sample": sample,
                "blocks": len(rs),
                "transactions": tx,
                "post_tps": ratio(tx * 1e9, post_ns),
                "post_ms": post_ns / 1e6,
                "pre_ms": pre_ns / 1e6,
                "reexec_pct": 100.0 * ratio(reexec, tx),
                "attempts": attempts,
                "discovered_conflicts": discovered,
                "forward_fallbacks": forward,
                "safety_replays": safety,
                "acg_dependency_edges": acg_deps,
                "acg_oracle_conflict_edges": acg_oracle_edges,
                "acg_preexec_tps": ratio(tx * 1e9, acg_pre_ns) if strategy == "cosmos-wasmd-symbgraph-rust" else 0.0,
                "acg_preexec_ms": acg_pre_ns / 1e6,
                "acg_plan_ms": acg_plan_ns / 1e6,
                "acg_worker_utilization": acg_util,
                "acg_max_active": acg_max_active,
                "vegeta_tx_concurrency": ratio(vegeta_work, vegeta_span),
                "vegeta_ready_ideal": ratio(vegeta_total_cost, vegeta_ready_cost),
                "aria_initial_concurrency": ratio(aria_work, aria_wall),
                "serial_equivalent": all(bool(r.get("serial_equivalent", False)) for r in rs),
            }
        )
    return out


def aggregate(samples: list[dict[str, Any]]) -> list[dict[str, Any]]:
    groups: dict[tuple[str, int], list[dict[str, Any]]] = defaultdict(list)
    for row in samples:
        groups[(row["strategy"], row["workers"])].append(row)

    base_tps_by_sample: dict[str, dict[int, float]] = defaultdict(dict)
    base_acg_pre_by_sample: dict[int, float] = {}
    for (strategy, workers), rs in groups.items():
        if workers != 1:
            continue
        for r in rs:
            base_tps_by_sample[strategy][int(r["sample"])] = float(r["post_tps"])
            if strategy == "cosmos-wasmd-symbgraph-rust":
                base_acg_pre_by_sample[int(r["sample"])] = float(r["acg_preexec_tps"])

    out: list[dict[str, Any]] = []
    for strategy in ORDER:
        for workers in sorted(w for (s, w) in groups if s == strategy):
            rs = groups[(strategy, workers)]
            post_tps_values = [float(r["post_tps"]) for r in rs]
            tps, tps_ci = mean_ci95(post_tps_values)
            scale_values = [
                ratio(float(r["post_tps"]), base_tps_by_sample[strategy].get(int(r["sample"]), 0.0))
                for r in rs
                if base_tps_by_sample[strategy].get(int(r["sample"]), 0.0)
            ]
            scale, scale_ci = mean_ci95(scale_values)
            acg_pre_tps = statistics.fmean(float(r["acg_preexec_tps"]) for r in rs)
            acg_pre_scale_values = []
            if strategy == "cosmos-wasmd-symbgraph-rust":
                acg_pre_scale_values = [
                    ratio(float(r["acg_preexec_tps"]), base_acg_pre_by_sample.get(int(r["sample"]), 0.0))
                    for r in rs
                    if base_acg_pre_by_sample.get(int(r["sample"]), 0.0)
                ]
            acg_pre_scale, acg_pre_scale_ci = mean_ci95(acg_pre_scale_values)
            out.append(
                {
                    "strategy": strategy,
                    "label": LABEL[strategy],
                    "workers": workers,
                    "samples": len(rs),
                    "transactions": rs[0]["transactions"],
                    "post_tps": tps,
                    "post_tps_ci95": tps_ci,
                    "post_scale_vs_1w": scale,
                    "post_scale_vs_1w_ci95": scale_ci,
                    "post_efficiency_pct": 100.0 * ratio(scale, workers),
                    "post_ms": statistics.fmean(float(r["post_ms"]) for r in rs),
                    "reexec_pct": statistics.fmean(float(r["reexec_pct"]) for r in rs),
                    "discovered_conflicts": statistics.fmean(float(r["discovered_conflicts"]) for r in rs),
                    "forward_fallbacks": statistics.fmean(float(r["forward_fallbacks"]) for r in rs),
                    "safety_replays": statistics.fmean(float(r["safety_replays"]) for r in rs),
                    "acg_dependency_edges": statistics.fmean(float(r["acg_dependency_edges"]) for r in rs),
                    "acg_oracle_conflict_edges": statistics.fmean(float(r["acg_oracle_conflict_edges"]) for r in rs),
                    "acg_preexec_tps": acg_pre_tps,
                    "acg_preexec_scale_vs_1w": acg_pre_scale,
                    "acg_preexec_scale_vs_1w_ci95": acg_pre_scale_ci,
                    "acg_preexec_efficiency_pct": 100.0 * ratio(acg_pre_scale, workers) if strategy == "cosmos-wasmd-symbgraph-rust" else 0.0,
                    "acg_worker_utilization": statistics.fmean(float(r["acg_worker_utilization"]) for r in rs),
                    "acg_max_active": max(int(r["acg_max_active"]) for r in rs),
                    "vegeta_tx_concurrency": statistics.fmean(float(r["vegeta_tx_concurrency"]) for r in rs),
                    "vegeta_ready_ideal": statistics.fmean(float(r["vegeta_ready_ideal"]) for r in rs),
                    "aria_initial_concurrency": statistics.fmean(float(r["aria_initial_concurrency"]) for r in rs),
                    "serial_equivalent": all(bool(r["serial_equivalent"]) for r in rs),
                }
            )
    return out


def gate(agg: list[dict[str, Any]], max_reexec_pct: float) -> list[str]:
    problems: list[str] = []
    for row in agg:
        if not row["serial_equivalent"]:
            problems.append(f"{row['label']} w={row['workers']} failed serial equivalence")
        if row["strategy"] == "cosmos-wasmd-direct-serial":
            continue
        if row["reexec_pct"] > max_reexec_pct:
            problems.append(
                f"{row['label']} w={row['workers']} reexec={row['reexec_pct']:.3f}% exceeds {max_reexec_pct:.3f}%"
            )
        if row["strategy"] in {"cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta"} and row["discovered_conflicts"] > 0:
            problems.append(
                f"{row['label']} w={row['workers']} discovered {row['discovered_conflicts']:.1f} conflicts/sample"
            )
        if row["strategy"] == "cosmos-wasmd-aria-fb" and row["forward_fallbacks"] > 0:
            problems.append(
                f"AriaFB w={row['workers']} has {row['forward_fallbacks']:.1f} Rule-2 fallbacks/sample"
            )
        if row["strategy"] == "cosmos-wasmd-symbgraph-rust":
            if row["acg_dependency_edges"] > 0:
                problems.append(
                    f"Rust-ACG w={row['workers']} planned {row['acg_dependency_edges']:.1f} dependency edges/sample"
                )
            if row["acg_oracle_conflict_edges"] > 0:
                problems.append(
                    f"Rust-ACG w={row['workers']} observed {row['acg_oracle_conflict_edges']:.1f} concrete conflict edges/sample"
                )
    return problems


def render(agg: list[dict[str, Any]], host_kind: str, max_reexec_pct: float, problems: list[str]) -> str:
    lines = [
        "ConflictLab native Wasmd zero-conflict upper bound",
        "",
        "Purpose:",
        "  Real ConflictLab CosmWasm/WasmVM execution with one unique BALANCES key per transaction.",
        "  This is the same Wasmd evaluator and the same Serial/BlockSTM/AriaFB/Vegeta/Rust-ACG implementations used by S1.",
        "  Synthetic evaluator compute is disabled; transaction cost comes from ConflictLab deterministic_work inside Wasm.",
        f"  Host classification: {host_kind}",
        "",
        "Post/replay scaling relative to each strategy's own 1-worker throughput:",
        f"{'system':<10} {'w':>3} {'post-tps':>11} {'scale':>8} {'eff':>8} {'reexec':>9} {'conflicts':>10}",
    ]
    for row in agg:
        conflicts = 0.0
        if row["strategy"] in {"cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta"}:
            conflicts = row["discovered_conflicts"]
        elif row["strategy"] == "cosmos-wasmd-symbgraph-rust":
            conflicts = row["acg_oracle_conflict_edges"]
        lines.append(
            f"{row['label']:<10} {row['workers']:>3} {row['post_tps']:>11.1f} "
            f"{row['post_scale_vs_1w']:>7.2f}x {row['post_efficiency_pct']:>7.1f}% "
            f"{row['reexec_pct']:>8.3f}% {conflicts:>10.1f}"
        )

    lines += [
        "",
        "Scheduler/runtime upper-bound diagnostics:",
        f"{'w':>3} {'ideal':>7} {'Vegeta-ready':>13} {'Vegeta-conc':>13} {'Aria-init':>11} {'ACG-pre-scale':>13} {'ACG-pre-eff':>11} {'ACG-util':>9} {'ACG-active':>10}",
    ]
    by = {(r["strategy"], r["workers"]): r for r in agg}
    workers = sorted({r["workers"] for r in agg})
    for w in workers:
        veg = by.get(("cosmos-wasmd-vegeta", w), {})
        aria = by.get(("cosmos-wasmd-aria-fb", w), {})
        acg = by.get(("cosmos-wasmd-symbgraph-rust", w), {})
        lines.append(
            f"{w:>3} {float(w):>6.2f}x {float(veg.get('vegeta_ready_ideal', 0.0)):>12.2f}x "
            f"{float(veg.get('vegeta_tx_concurrency', 0.0)):>12.2f}x "
            f"{float(aria.get('aria_initial_concurrency', 0.0)):>10.2f}x "
            f"{float(acg.get('acg_preexec_scale_vs_1w', 0.0)):>12.2f}x "
            f"{float(acg.get('acg_preexec_efficiency_pct', 0.0)):>10.1f}% "
            f"{float(acg.get('acg_worker_utilization', 0.0)):>8.3f} "
            f"{int(acg.get('acg_max_active', 0)):>10d}"
        )

    lines += [
        "",
        "Zero-conflict acceptance gate:",
        f"  allowed intrinsic reexecution: <= {max_reexec_pct:.3f}%",
    ]
    if problems:
        lines.append("  FAIL")
        lines.extend(f"    - {problem}" for problem in problems)
    else:
        lines.append("  PASS: no concrete dependency/fallback signal was observed beyond the configured tolerance")

    lines += [
        "",
        "How to read the WSL question:",
        "  * In this workload the structural worker ceiling is exactly w: there are far more independent transactions than workers.",
        "  * If BlockSTM, AriaFB initial execution, Vegeta replay, and Rust-ACG preexecution all flatten similarly below w, the shared Wasmd/WasmVM/host environment is the leading bottleneck rather than S1 conflicts.",
        "  * If the baselines approach w but Rust-ACG preexecution does not, the bottleneck is local to ACG.",
        "  * A WSL-only run cannot prove WSL is causal. The decisive test is to run this exact script on native Linux on the same machine and compare the scale/efficiency columns.",
        "  * Rust-ACG replay-x itself is not a hardware scaling metric because useful work is intentionally moved before consensus; use ACG-pre-scale for that diagnosis.",
    ]
    return "\n".join(lines) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--records", required=True, type=Path)
    ap.add_argument("--output", required=True, type=Path)
    ap.add_argument("--json-output", type=Path)
    ap.add_argument("--host-kind", default="unknown")
    ap.add_argument("--max-reexec-pct", type=float, default=0.5)
    args = ap.parse_args()

    rows = read_jsonl(args.records)
    present = {str(r.get("strategy", "")) for r in rows}
    missing = [LABEL[s] for s in ORDER if s not in present]
    if missing:
        raise SystemExit("missing strategies: " + ", ".join(missing))
    samples = sample_metrics(rows)
    agg = aggregate(samples)
    if not any(r["workers"] == 1 for r in agg):
        raise SystemExit("upper-bound scaling experiment requires a 1-worker baseline")
    problems = gate(agg, args.max_reexec_pct)
    text = render(agg, args.host_kind, args.max_reexec_pct, problems)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text, encoding="utf-8")
    if args.json_output:
        args.json_output.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "host_kind": args.host_kind,
                    "max_reexec_pct": args.max_reexec_pct,
                    "gate_passed": not problems,
                    "gate_problems": problems,
                    "rows": agg,
                    "per_sample": samples,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
    print(text, end="")
    if problems:
        raise SystemExit(2)


if __name__ == "__main__":
    main()
