#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import json
import statistics
from collections import defaultdict
from pathlib import Path


def median(values):
    return statistics.median(values) if values else 0.0


def profile_key(metric, scale):
    return (str(metric), float(scale))


def main():
    ap = argparse.ArgumentParser(description="Summarize EVM-cost calibrated native S3 replay scaling.")
    ap.add_argument("--records", required=True)
    ap.add_argument("--profiles-root", required=True)
    ap.add_argument("--output-dir", required=True)
    args = ap.parse_args()

    records = [json.loads(line) for line in open(args.records, encoding="utf-8") if line.strip()]
    grouped = defaultdict(list)
    for row in records:
        grouped[(
            row.get("compute_calibration_metric", "none"),
            float(row.get("compute_scale", 0.0) or 0.0),
            row["workers"], row["sample"], row["strategy"],
        )].append(row)

    sample_rows = []
    for (metric, scale, workers, sample, strategy), rows in sorted(grouped.items()):
        serial = sum(r["matched_serial_nanos"] for r in rows)
        total = sum(r["strategy_total_nanos"] for r in rows)
        pre = sum(r["preexecution_nanos"] for r in rows)
        replay = sum(r["reconciliation_nanos"] for r in rows)
        post = sum(r["post_consensus_nanos"] for r in rows)
        sample_rows.append({
            "metric": metric,
            "scale": scale,
            "workers": workers,
            "sample": sample,
            "strategy": strategy,
            "blocks": len(rows),
            "transactions": sum(r["transactions"] for r in rows),
            "matched_serial_ms": serial / 1e6,
            "strategy_total_ms": total / 1e6,
            "preexecution_ms": pre / 1e6,
            "reconciliation_ms": replay / 1e6,
            "post_consensus_ms": post / 1e6,
            "active_speedup": serial / total if total else 0.0,
            "preexecution_speedup": serial / pre if pre else 0.0,
            "replay_speedup": serial / replay if replay else 0.0,
            "post_speedup": serial / post if post else 0.0,
            "serial_equivalent": all(r.get("serial_equivalent") for r in rows),
        })

    serial_controls = {
        (r["metric"], r["scale"], r["workers"], r["sample"]): r["active_speedup"]
        for r in sample_rows if r["strategy"] == "serial"
    }
    for row in sample_rows:
        control = serial_controls.get((row["metric"], row["scale"], row["workers"], row["sample"]), 0.0)
        row["normalized_active_speedup"] = row["active_speedup"] / control if control else 0.0

    by = defaultdict(list)
    for row in sample_rows:
        by[(row["metric"], row["scale"], row["workers"], row["strategy"])].append(row)
    scaling = []
    for (metric, scale, workers, strategy), rows in sorted(by.items()):
        scaling.append({
            "metric": metric,
            "scale": scale,
            "workers": workers,
            "strategy": strategy,
            "samples": len(rows),
            "active_speedup_median": median([r["active_speedup"] for r in rows]),
            "normalized_active_speedup_median": median([r["normalized_active_speedup"] for r in rows]),
            "preexecution_speedup_median": median([r["preexecution_speedup"] for r in rows]),
            "replay_speedup_median": median([r["replay_speedup"] for r in rows]),
            "post_speedup_median": median([r["post_speedup"] for r in rows]),
            "strategy_total_ms_median": median([r["strategy_total_ms"] for r in rows]),
            "serial_equivalent": all(r["serial_equivalent"] for r in rows),
        })

    fidelity = []
    for path in sorted(Path(args.profiles_root).glob("*/cost-fidelity/summary.json")):
        summary = json.loads(path.read_text(encoding="utf-8"))
        calibration = summary.get("compute_calibration", {})
        corr = summary.get("correlation", {})
        fidelity.append({
            "profile": path.parent.parent.name,
            "metric": calibration.get("metric", "none"),
            "scale": float(calibration.get("scale", 0.0) or 0.0),
            "native_vs_steps_pearson": corr.get("native_vs_steps_pearson", 0.0),
            "native_vs_steps_spearman": corr.get("native_vs_steps_spearman", 0.0),
            "native_vs_gas_pearson": corr.get("native_vs_gas_pearson", 0.0),
            "native_vs_gas_spearman": corr.get("native_vs_gas_spearman", 0.0),
            "critical_path_native_overweight_ratio": summary.get("critical_path_native_overweight_ratio"),
            "native_median_us": summary.get("native_execution_us", {}).get("median", 0.0),
            "compute_iterations_total": summary.get("compute_iterations_total", 0),
            "missing_source_transactions": summary.get("missing_source_transactions", 0),
        })

    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)
    for name, data in [
        ("per-sample.csv", sample_rows),
        ("scaling-summary.csv", scaling),
        ("fidelity-summary.csv", fidelity),
    ]:
        with (out / name).open("w", newline="", encoding="utf-8") as f:
            if data:
                writer = csv.DictWriter(f, fieldnames=list(data[0]))
                writer.writeheader()
                writer.writerows(data)
    (out / "scaling-summary.json").write_text(json.dumps(scaling, indent=2) + "\n", encoding="utf-8")
    (out / "fidelity-summary.json").write_text(json.dumps(fidelity, indent=2) + "\n", encoding="utf-8")

    fidelity_by_key = {profile_key(r["metric"], r["scale"]): r for r in fidelity}
    lines = ["Native S3 EVM-cost calibration sweep", ""]
    for key in sorted({(r["metric"], r["scale"]) for r in scaling}):
        metric, scale = key
        f = fidelity_by_key.get(profile_key(metric, scale))
        if f:
            lines.append(
                f"profile metric={metric:<8} scale={scale:<4g} "
                f"steps-rho={f['native_vs_steps_spearman']:.3f} gas-rho={f['native_vs_gas_spearman']:.3f} "
                f"critical-overweight={f['critical_path_native_overweight_ratio']:.3f} "
                f"native-med={f['native_median_us']:.1f}us"
            )
        for row in [r for r in scaling if r["metric"] == metric and r["scale"] == scale]:
            lines.append(
                f"  workers={row['workers']:>2} strategy={row['strategy']:<12} "
                f"active={row['active_speedup_median']:.3f}x net={row['normalized_active_speedup_median']:.3f}x "
                f"pre={row['preexecution_speedup_median']:.3f}x replay={row['replay_speedup_median']:.3f}x "
                f"post={row['post_speedup_median']:.3f}x serial-eq={row['serial_equivalent']}"
            )
        lines.append("")
    lines += [
        "Interpretation:",
        "  Increasing scale adds deterministic CPU work distributed by frozen source steps/gas without changing accesses.",
        "  A fidelity improvement should move rank correlation upward and critical-overweight toward 1.0.",
        "  If exact-direct scaling rises with compute intensity, the semantic-only workload was too fine-grained to amortize scheduling.",
        "  'net' divides each strategy active speedup by the same-profile serial strategy control to reduce paired-run order bias.",
    ]
    (out / "summary.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
