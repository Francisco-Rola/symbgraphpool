#!/usr/bin/env python3
import argparse
import csv
import json
import math
from collections import defaultdict
from pathlib import Path


def load(path: Path):
    rows = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def ms(ns):
    return ns / 1_000_000.0


def pct(num, den):
    return 0.0 if den == 0 else 100.0 * num / den


def aggregate(rows):
    serial = sum(r["matched_serial_nanos"] for r in rows)
    wall = sum(r["strategy_total_nanos"] for r in rows)
    tx = sum(r["transactions"] for r in rows)
    replays = sum(r["reexecutions"] for r in rows)
    workers = rows[0]["workers"]
    pre = sum(r.get("symb_preexecution_nanos", 0) for r in rows)
    busy = sum(r.get("symb_spec_execution_nanos", 0) + r.get("symb_delta_capture_nanos", 0) for r in rows)
    point_reads = sum(r.get("symb_mvcc_point_reads", 0) for r in rows)
    version_hits = sum(r.get("symb_mvcc_version_hits", 0) for r in rows)
    return {
        "workers": workers,
        "samples": len(set(r["sample"] for r in rows)),
        "blocks": len(rows),
        "transactions": tx,
        "active_ms": ms(wall),
        "net_x": 0.0 if wall == 0 else serial / wall,
        "replay_pct": pct(replays, tx),
        "plan_ms": ms(sum(r.get("symb_plan_nanos", 0) for r in rows)),
        "preexec_ms": ms(pre),
        "branch_ms": ms(sum(r.get("symb_branch_create_nanos", 0) for r in rows)),
        "visibility_ms": ms(sum(r.get("symb_visibility_nanos", 0) for r in rows)),
        "validation_ms": ms(sum(r.get("symb_validation_nanos", 0) for r in rows)),
        "feedback_build_ms": ms(sum(r.get("symb_feedback_build_nanos", 0) for r in rows)),
        "rust_feedback_ms": ms(sum(r.get("symb_rust_feedback_nanos", 0) for r in rows)),
        "reconcile_ms": ms(sum(r.get("symb_reconciliation_nanos", 0) for r in rows)),
        "replay_exec_ms": ms(sum(r.get("symb_replay_execution_nanos", 0) for r in rows)),
        "mvcc_publish_ms": ms(sum(r.get("symb_mvcc_publish_nanos", 0) for r in rows)),
        "worker_util_pct": pct(busy, pre * workers),
        "dependency_edges_per_block": sum(r.get("symb_dependency_edges", 0) for r in rows) / len(rows),
        "feedback_pairs_per_block": sum(r.get("symb_feedback_pairs", 0) for r in rows) / len(rows),
        "initial_ready_avg": sum(r.get("symb_initial_ready", 0) for r in rows) / len(rows),
        "max_ready_avg": sum(r.get("symb_max_ready", 0) for r in rows) / len(rows),
        "ready_width_avg": sum(r.get("symb_average_ready", 0.0) for r in rows) / len(rows),
        "max_active_avg": sum(r.get("symb_max_active", 0) for r in rows) / len(rows),
        "critical_path_tx_avg": sum(r.get("symb_critical_path_tx", 0) for r in rows) / len(rows),
        "dag_parallelism_avg": sum(r.get("symb_dag_parallelism", 0.0) for r in rows) / len(rows),
        "mvcc_point_reads": point_reads,
        "mvcc_hit_pct": pct(version_hits, point_reads),
        "mvcc_range_reads": sum(r.get("symb_mvcc_range_reads", 0) for r in rows),
        "mvcc_overlay_keys": sum(r.get("symb_mvcc_range_overlay_keys", 0) for r in rows),
    }


def baseline_speedups(rows):
    grouped = defaultdict(list)
    for r in rows:
        if r["strategy"] in {"cosmos-wasmd-block-stm", "cosmos-wasmd-vegeta"}:
            grouped[(r["strategy"], r["workers"])].append(r)
    result = {}
    for key, items in grouped.items():
        serial = sum(r["matched_serial_nanos"] for r in items)
        wall = sum(r["strategy_total_nanos"] for r in items)
        result[key] = 0.0 if wall == 0 else serial / wall
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--input-root", required=True)
    ap.add_argument("--output-dir", required=True)
    args = ap.parse_args()
    root = Path(args.input_root)
    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)

    results = []
    comparison = {}
    variants = sorted(p for p in root.iterdir() if p.is_dir())
    for variant_dir in variants:
        records = variant_dir / "records.jsonl"
        if not records.exists():
            continue
        all_rows = load(records)
        comparison[variant_dir.name] = baseline_speedups(all_rows)
        symb = [r for r in all_rows if r["strategy"] == "cosmos-wasmd-symbgraph-rust"]
        grouped = defaultdict(list)
        for row in symb:
            grouped[row["workers"]].append(row)
        for workers, rows in sorted(grouped.items()):
            item = aggregate(rows)
            item["variant"] = variant_dir.name
            item["runner_variant"] = rows[0].get("symbgraph_variant", "")
            item["blockstm_x"] = comparison[variant_dir.name].get(("cosmos-wasmd-block-stm", workers), math.nan)
            item["vegeta_x"] = comparison[variant_dir.name].get(("cosmos-wasmd-vegeta", workers), math.nan)
            results.append(item)

    legacy = {(r["workers"]): r for r in results if r["variant"] == "legacy"}
    for r in results:
        base = legacy.get(r["workers"])
        r["vs_legacy_wall_pct"] = 0.0 if not base else pct(base["active_ms"] - r["active_ms"], base["active_ms"])

    fields = [
        "variant", "runner_variant", "workers", "samples", "blocks", "transactions",
        "active_ms", "net_x", "vs_legacy_wall_pct", "blockstm_x", "vegeta_x", "replay_pct",
        "plan_ms", "preexec_ms", "branch_ms", "visibility_ms", "validation_ms",
        "feedback_build_ms", "rust_feedback_ms", "reconcile_ms", "replay_exec_ms", "mvcc_publish_ms",
        "worker_util_pct", "dependency_edges_per_block", "feedback_pairs_per_block",
        "initial_ready_avg", "max_ready_avg", "ready_width_avg", "max_active_avg", "critical_path_tx_avg", "dag_parallelism_avg",
        "mvcc_point_reads", "mvcc_hit_pct", "mvcc_range_reads", "mvcc_overlay_keys",
    ]
    with (out / "ablation.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for row in results:
            w.writerow({k: row.get(k) for k in fields})
    with (out / "ablation.json").open("w") as f:
        json.dump({"rows": results}, f, indent=2)
        f.write("\n")

    lines = [
        "Wasmd Rust-ACG optimization ablation",
        "",
        "active-ms is the summed 101-block scheduler wall time. vs-legacy is positive when the optimization is faster.",
        "phase columns are summed instrumented Rust-ACG costs; spec worker work is intentionally not shown as wall time.",
        "",
        f"{'variant':14} {'w':>2} {'active-ms':>10} {'net-x':>7} {'vs-legacy':>10} {'replay%':>8} {'vis-ms':>9} {'valid-ms':>9} {'fb-ms':>9} {'util%':>7} {'deps/b':>8} {'pairs/b':>8} {'ready':>7} {'cp-tx':>6} {'dag-x':>7}",
    ]
    for r in results:
        lines.append(
            f"{r['variant']:14} {r['workers']:2d} {r['active_ms']:10.1f} {r['net_x']:7.3f} "
            f"{r['vs_legacy_wall_pct']:9.1f}% {r['replay_pct']:7.2f}% {r['visibility_ms']:9.1f} "
            f"{r['validation_ms']:9.1f} {r['feedback_build_ms']:9.1f} {r['worker_util_pct']:6.1f}% "
            f"{r['dependency_edges_per_block']:8.1f} {r['feedback_pairs_per_block']:8.1f} "
            f"{r['ready_width_avg']:7.2f} {r['critical_path_tx_avg']:6.2f} {r['dag_parallelism_avg']:7.2f}"
        )
    lines += ["", "Reference scheduler speedups from the same Wasmd runs:"]
    for r in results:
        lines.append(f"  {r['variant']} w={r['workers']}: BlockSTM={r['blockstm_x']:.3f}x Vegeta={r['vegeta_x']:.3f}x Rust-ACG={r['net_x']:.3f}x")
    (out / "ablation.txt").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
