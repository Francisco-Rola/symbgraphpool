#!/usr/bin/env python3
"""Build machine-readable evaluation tables from frozen artifacts."""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def plan_stats(path: Path):
    blocks = transactions = 0
    try:
        with path.open(encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                block = json.loads(line)
                blocks += 1
                transactions += len(block.get("transactions") or [])
    except OSError:
        pass
    return blocks, transactions


def read_csv(path: Path):
    if not path.is_file():
        return []
    with path.open(newline="", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def get(data, *keys, default=None):
    value = data
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            return default
        value = value[key]
    return value


def pct(value):
    return "" if value is None else f"{100 * float(value):.2f}"


def write_csv(path: Path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    columns = []
    for row in rows:
        for key in row:
            if key not in columns:
                columns.append(key)
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=columns)
        writer.writeheader()
        writer.writerows(rows)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--result-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)

    s1_ready = load(ROOT / "benchmarks/corpora/vegeta-ethereum/s1/native-plan/readiness.json")
    s4_ready = load(ROOT / "benchmarks/corpora/vegeta-ethereum/s4/native-plan/readiness.json")
    topology = load(
        args.result_root / "16-translation-fidelity/topology/native-topology-fidelity.json"
    )
    cost = load(args.result_root / "16-translation-fidelity/cost/summary.json")

    workloads = []
    for name, slug, provenance, oracle, ready in [
        ("S1-derived Wasmd", "s1", "Ethereum S1, translated", "No", s1_ready),
        ("S3-derived Wasmd", "s3", "Ethereum S3 exact trace, translated", "Yes", {}),
        ("S4-derived Wasmd", "s4", "Ethereum S4, translated", "No", s4_ready),
    ]:
        blocks, transactions = plan_stats(
            ROOT
            / f"benchmarks/corpora/vegeta-ethereum/{slug}/native-execution/execution-plan.jsonl"
        )
        if slug == "s1":
            conflict = get(
                ready,
                "common_gates",
                "reviewed_state_touch_conflict_coverage",
                "value",
            )
            contention = get(
                ready,
                "profiles",
                "scheduler-fidelity",
                "gates",
                "successful_reviewed_conflict_participant_tx_coverage",
                "value",
            )
            semantic = get(
                ready,
                "profiles",
                "semantic-replay",
                "gates",
                "successful_reviewed_all_tx_coverage",
                "value",
            )
        elif slug == "s4":
            conflict = get(ready, "metrics", "semantic_conflict")
            contention = get(ready, "metrics", "contention_tx")
            semantic = get(ready, "metrics", "semantic_tx")
        else:
            conflict = contention = semantic = None

        topology_precision = (
            pct(get(topology, "conflict_pairs", "precision")) if slug == "s3" else "—"
        )
        topology_recall = (
            pct(get(topology, "conflict_pairs", "recall")) if slug == "s3" else "—"
        )
        native_steps_spearman = get(cost, "correlation", "native_vs_steps_spearman")
        cost_spearman = (
            f"{float(native_steps_spearman):.3f}"
            if slug == "s3" and native_steps_spearman is not None
            else "—"
        )
        workloads.append(
            {
                "workload": name,
                "provenance": provenance,
                "blocks": blocks or "—",
                "transactions": transactions or "—",
                "selector_conflict_coverage_pct": pct(conflict)
                or ("exact audit" if slug == "s3" else "—"),
                "conflict_participant_tx_pct": pct(contention)
                or ("exact audit" if slug == "s3" else "—"),
                "all_tx_semantic_pct": pct(semantic)
                or ("exact audit" if slug == "s3" else "—"),
                "exact_topology_precision_pct": topology_precision,
                "exact_topology_recall_pct": topology_recall,
                "native_steps_spearman": cost_spearman,
                "exact_access_oracle": oracle,
            }
        )

    workloads += [
        {
            "workload": "MiniWarehouse",
            "provenance": "Native generated",
            "blocks": "configurable",
            "transactions": "configurable",
            "selector_conflict_coverage_pct": "native",
            "conflict_participant_tx_pct": "native",
            "all_tx_semantic_pct": "native",
            "exact_topology_precision_pct": "—",
            "exact_topology_recall_pct": "—",
            "native_steps_spearman": "—",
            "exact_access_oracle": "Serial state",
        },
        {
            "workload": "NativeMix",
            "provenance": "Native CW20/CW721/AMM",
            "blocks": "configurable",
            "transactions": "configurable",
            "selector_conflict_coverage_pct": "native",
            "conflict_participant_tx_pct": "native",
            "all_tx_semantic_pct": "native",
            "exact_topology_precision_pct": "—",
            "exact_topology_recall_pct": "—",
            "native_steps_spearman": "—",
            "exact_access_oracle": "Serial state",
        },
        {
            "workload": "ConflictLab",
            "provenance": "Controlled native Wasm",
            "blocks": "configurable",
            "transactions": "configurable",
            "selector_conflict_coverage_pct": "known by construction",
            "conflict_participant_tx_pct": "known by construction",
            "all_tx_semantic_pct": "native",
            "exact_topology_precision_pct": "—",
            "exact_topology_recall_pct": "—",
            "native_steps_spearman": "—",
            "exact_access_oracle": "Serial state",
        },
    ]
    write_csv(args.output_dir / "table1-workloads-fidelity.csv", workloads)

    source = args.result_root / "12-semantics/aggregate/summary-wide.csv"
    acceptance = load(args.result_root / "12-semantics/acceptance.json")
    semantics = []
    for row in read_csv(source):
        def value(key):
            try:
                return float(row.get(key) or 0)
            except (TypeError, ValueError):
                return 0.0

        semantics.append(
            {
                "mode": row.get("mode", ""),
                "operation_mix": row.get("param.operation_mix", ""),
                "contention": row.get("param.contention", ""),
                "n": row.get("n", "") or row.get("throughput_speedup.n", ""),
                "throughput_speedup": f"{value('throughput_speedup.mean'):.3f}",
                "replayed_transactions": f"{value('replayed_transactions.mean'):.1f}",
                "candidate_misses": f"{value('candidate_misses.mean'):.1f}",
                "accepted": "yes"
                if acceptance.get("status") == "accepted"
                else str(acceptance.get("status") or "unknown"),
            }
        )
    write_csv(args.output_dir / "table2-semantics-correctness.csv", semantics)
    print(args.output_dir)


if __name__ == "__main__":
    main()
