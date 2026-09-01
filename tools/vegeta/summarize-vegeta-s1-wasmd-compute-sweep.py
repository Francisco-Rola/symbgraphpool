#!/usr/bin/env python3
"""Summarize the cached S1 Wasmd gas-weight compute-scale sweep."""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

STRATEGIES = {
    "cosmos-wasmd-direct-serial": "Serial",
    "cosmos-wasmd-block-stm": "BlockSTM",
    "cosmos-wasmd-aria-fb": "AriaFB",
    "cosmos-wasmd-vegeta": "Vegeta",
    "cosmos-wasmd-symbgraph-rust": "Rust-ACG",
}
NON_SERIAL = ["BlockSTM", "AriaFB", "Vegeta", "Rust-ACG"]


def f(row: dict[str, str], key: str) -> float:
    return float(row.get(key) or 0.0)


def linear_fit(xs: list[float], ys: list[float]) -> tuple[float, float, float]:
    if len(xs) < 2:
        return 0.0, 0.0, 0.0
    xm = sum(xs) / len(xs)
    ym = sum(ys) / len(ys)
    denom = sum((x - xm) ** 2 for x in xs)
    slope = sum((x - xm) * (y - ym) for x, y in zip(xs, ys)) / denom if denom else 0.0
    intercept = ym - slope * xm
    pred = [intercept + slope * x for x in xs]
    ss_res = sum((y - p) ** 2 for y, p in zip(ys, pred))
    ss_tot = sum((y - ym) ** 2 for y in ys)
    r2 = 1.0 - ss_res / ss_tot if ss_tot else 1.0
    return intercept, slope, r2


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", required=True)
    ap.add_argument("--scales", required=True)
    ap.add_argument("--workers", type=int, required=True)
    ap.add_argument("--blocks", type=int, required=True)
    ap.add_argument("--output-json", required=True)
    ap.add_argument("--output-text", required=True)
    args = ap.parse_args()

    root = Path(args.root)
    scales = [float(x) for x in args.scales.split(",") if x.strip()]
    profiles = []
    weight_signatures = set()
    iterations = None
    iter_path = root / "iterations-per-ns.txt"
    if iter_path.exists():
        iterations = float(iter_path.read_text().strip())

    for scale in scales:
        label = f"scale-{('%g' % scale).replace('.', 'p')}"
        d = root / label
        csv_path = d / "summary" / "summary.csv"
        weights_path = d / "compute-weights-summary.json"
        if not csv_path.exists() or not weights_path.exists():
            raise SystemExit(f"missing completed scale output: {d}")
        weights = json.loads(weights_path.read_text())
        sig = (
            weights.get("compute_metric"), int(weights.get("transactions", -1)),
            int(weights.get("weighted_transactions", -1)), int(weights.get("missing_source_transactions", -1)),
            int(weights.get("source_compute_units_total", -1)), bool(weights.get("fallback_plan_gas")),
        )
        weight_signatures.add(sig)
        rows = list(csv.DictReader(csv_path.open(encoding="utf-8")))
        chosen = {STRATEGIES[r["strategy"]]: r for r in rows if r.get("strategy") in STRATEGIES and int(r["workers"]) == args.workers}
        if set(chosen) != set(STRATEGIES.values()):
            raise SystemExit(f"incomplete strategy summary for scale={scale}: {sorted(chosen)}")
        if not all(str(r.get("serial_equivalent", "")).lower() in {"true", "1"} for r in chosen.values()):
            raise SystemExit(f"state-equivalence failure at scale={scale}")
        tput = {name: f(row, "throughput_speedup") for name, row in chosen.items()}
        ranking = sorted(NON_SERIAL, key=lambda name: (-tput[name], name))
        serial = chosen["Serial"]
        profiles.append({
            "scale": scale,
            "serial_post_ms": f(serial, "post_ms"),
            "serial_wall_ms": f(serial, "wall_ms"),
            "consensus_window_ms": f(serial, "consensus_window_ms"),
            "throughput_speedup": tput,
            "throughput_tps": {name: f(row, "throughput_tps") for name, row in chosen.items()},
            "replay_pct": {name: f(row, "replay_pct") for name, row in chosen.items()},
            "ranking": ranking,
        })

    if len(weight_signatures) != 1:
        raise SystemExit(f"compute-weight provenance changed across scales: {sorted(weight_signatures)}")
    metric, txs, weighted, missing, units, fallback = next(iter(weight_signatures))
    if metric != "gas_used" or missing != 0 or txs != weighted:
        raise SystemExit(f"unexpected S1 compute weights: metric={metric} tx={txs} weighted={weighted} missing={missing}")

    xs = [p["scale"] for p in profiles]
    ys = [p["serial_post_ms"] for p in profiles]
    intercept, slope, r2 = linear_fit(xs, ys)
    for p in profiles:
        predicted = intercept + slope * p["scale"]
        supplement = max(0.0, slope * p["scale"])
        p["fitted_supplement_share"] = supplement / predicted if predicted > 0 else 0.0

    by_scale = {p["scale"]: p for p in profiles}
    candidate = by_scale.get(4.0)
    neighbor_scales = [s for s in (2.0, 4.0, 8.0) if s in by_scale]
    top_neighbors = [by_scale[s]["ranking"][0] for s in neighbor_scales]
    candidate_diag = None
    if candidate:
        candidate_diag = {
            "scale": 4.0,
            "fitted_supplement_share": candidate["fitted_supplement_share"],
            "ranking": candidate["ranking"],
            "throughput_speedup": candidate["throughput_speedup"],
            "same_top_strategy_across_2x_4x_8x": len(set(top_neighbors)) == 1 if len(top_neighbors) >= 2 else None,
            "top_strategy_neighbors": dict(zip((str(s) for s in neighbor_scales), top_neighbors)),
            "status": "candidate-not-auto-selected",
        }

    out = {
        "schema_version": 1,
        "dataset": "vegeta-s1-wasmd-compute-calibration",
        "blocks": args.blocks,
        "workers": args.workers,
        "iterations_per_nano": iterations,
        "weights": {
            "metric": metric, "transactions": txs, "weighted_transactions": weighted,
            "missing": missing, "units": units, "fallback_plan_gas": fallback,
            "relative_distribution_invariant_across_scales": True,
        },
        "serial_linear_fit": {
            "model": "serial_post_ms ~= fixed_ms + scale * supplemental_ms_per_scale",
            "fixed_ms": intercept,
            "supplemental_ms_per_scale": slope,
            "r2": r2,
            "note": "diagnostic decomposition; it estimates fixed native Wasm/runtime cost from the scale sweep and is not a source-EVM timing measurement",
        },
        "profiles": profiles,
        "candidate_4x": candidate_diag,
        "methodology": {
            "purpose": "check that source-gas weighted deterministic CPU creates useful transaction granularity without changing state dependencies",
            "selection_rule": "diagnostic, not threshold-seeking; compare fitted compute share and scheduler ranking/speedup stability across neighboring scales",
            "topology_changed": False,
            "source_weight_ranking_changed_by_scale": False,
        },
    }
    Path(args.output_json).write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")

    lines = [
        "Vegeta S1 Wasmd compute calibration sweep",
        "",
        f"blocks: {args.blocks}",
        f"workers: {args.workers}",
        f"gas_used weights: tx={txs} weighted={weighted} missing={missing} units={units}",
        f"pinned iterations/ns: {iterations if iterations is not None else 'unknown'}",
        "relative gas_used weight distribution: invariant across scales (scalar multiplier only)",
        "",
        "scale  serial-post-ms  fitted-compute-share  BlockSTM-x  AriaFB-x  Vegeta-x  Rust-ACG-x  ranking",
    ]
    for p in profiles:
        t = p["throughput_speedup"]
        lines.append(
            f"{p['scale']:>5g}  {p['serial_post_ms']:>14.1f}  {100*p['fitted_supplement_share']:>19.1f}%"
            f"  {t['BlockSTM']:>10.3f}  {t['AriaFB']:>8.3f}  {t['Vegeta']:>8.3f}  {t['Rust-ACG']:>10.3f}  {' > '.join(p['ranking'])}"
        )
    lines += [
        "",
        "Serial cost decomposition (diagnostic linear fit):",
        f"  estimated fixed native Wasm/runtime cost: {intercept:.1f} ms",
        f"  estimated supplemental cost per 1x scale: {slope:.1f} ms",
        f"  linear-fit R^2: {r2:.4f}",
    ]
    if candidate_diag:
        lines += [
            "",
            "4x candidate diagnostics:",
            f"  fitted supplemental share: {100*candidate_diag['fitted_supplement_share']:.1f}%",
            f"  ranking: {' > '.join(candidate_diag['ranking'])}",
            f"  same top strategy across available 2x/4x/8x neighbors: {candidate_diag['same_top_strategy_across_2x_4x_8x']}",
            "  status: candidate-not-auto-selected",
        ]
    lines += [
        "",
        "Interpretation:",
        "  - Scaling is state-free and does not change the reviewed S1 dependency topology.",
        "  - The gas_used weight ordering is identical at every scale; only deterministic CPU intensity changes.",
        "  - Prefer a scale where fixed Wasm/runtime cost is no longer overwhelmingly dominant, while synthetic compute is not the whole service time.",
        "  - Scheduler ordering and throughput gains should be reasonably stable between neighboring scales.",
        "  - 4x remains the inherited candidate until these measurements are reviewed; this script deliberately does not auto-select a publication scale.",
    ]
    Path(args.output_text).write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
