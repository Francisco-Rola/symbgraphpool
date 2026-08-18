#!/usr/bin/env python3
import argparse
import csv
import json
import statistics
from collections import defaultdict
from pathlib import Path

MODES = [
    "serial",
    "aria-fb",
    "vegeta",
    "exact-access",
    "static",
    "probability-only",
    "cost-aware",
]


def safe_ratio(numerator, denominator):
    if numerator is None or denominator in (None, 0):
        return None
    return float(numerator) / float(denominator)


def values_only(values):
    return [float(value) for value in values if value is not None]


def median(values):
    values = values_only(values)
    return statistics.median(values) if values else None


def min_med_max(values):
    values = values_only(values)
    if not values:
        return None
    return min(values), statistics.median(values), max(values)


def fmt_x(value):
    return "n/a" if value is None else f"{value:.2f}x"


def fmt_range(values):
    stats = min_med_max(values)
    if stats is None:
        return "n/a"
    low, med, high = stats
    return f"{low:.2f}/{med:.2f}/{high:.2f}x"


def fmt_ms(value):
    return "n/a" if value is None else f"{value / 1_000_000:.2f}"


def match_key(record):
    metadata = record.get("metadata", {})
    params = metadata.get("parameters", {})
    return (
        metadata.get("experiment_id"),
        metadata.get("workload"),
        metadata.get("workers"),
        metadata.get("seed"),
        tuple(sorted((str(key), str(value)) for key, value in params.items())),
    )


def build_serial_index(records):
    grouped = defaultdict(list)
    for record in records:
        if record.get("metadata", {}).get("mode") == "serial":
            grouped[match_key(record)].append(record)
    return grouped


def matched_serial_speedup(record, serial_index):
    matches = serial_index.get(match_key(record), [])
    if len(matches) != 1:
        return None
    serial_wall = matches[0].get("pipeline_timing", {}).get("total_adaptive_block_nanos")
    strategy_wall = record.get("pipeline_timing", {}).get("total_adaptive_block_nanos")
    return safe_ratio(serial_wall, strategy_wall)


def parameter_value(records, name):
    values = {
        str(record.get("metadata", {}).get("parameters", {}).get(name, "n/a"))
        for record in records
    }
    if len(values) == 1:
        return next(iter(values))
    return "mixed:" + ",".join(sorted(values))


def write_matched_csv(path, records, serial_index):
    rows = []
    for record in sorted(
        records,
        key=lambda r: (
            str(r.get("metadata", {}).get("parameters", {}).get("contention", "n/a")),
            str(r.get("metadata", {}).get("mode", "")),
            int(r.get("metadata", {}).get("seed", 0)),
        ),
    ):
        metadata = record.get("metadata", {})
        params = metadata.get("parameters", {})
        serial_matches = serial_index.get(match_key(record), [])
        serial_wall = (
            serial_matches[0].get("pipeline_timing", {}).get("total_adaptive_block_nanos")
            if len(serial_matches) == 1
            else None
        )
        strategy_wall = record.get("pipeline_timing", {}).get("total_adaptive_block_nanos")
        rows.append(
            {
                "experiment_id": metadata.get("experiment_id"),
                "workload": metadata.get("workload"),
                "mode": metadata.get("mode"),
                "workers": metadata.get("workers"),
                "seed": metadata.get("seed"),
                "contention": params.get("contention"),
                "hot_account_probability_bps": params.get("hot_account_probability_bps"),
                "serial_wall_nanos": serial_wall,
                "strategy_wall_nanos": strategy_wall,
                "matched_serial_speedup": matched_serial_speedup(record, serial_index),
            }
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("records")
    parser.add_argument("--output")
    parser.add_argument("--matched-output")
    args = parser.parse_args()

    records = [
        json.loads(line)
        for line in Path(args.records).read_text().splitlines()
        if line.strip()
    ]
    groups = defaultdict(list)
    for record in records:
        groups[record["metadata"]["mode"]].append(record)

    serial_index = build_serial_index(records)
    failures = []
    expected_keys = {match_key(record) for record in records}
    missing_serial = [key for key in expected_keys if key not in serial_index]
    duplicate_serial = [key for key, rows in serial_index.items() if len(rows) != 1]
    if missing_serial:
        failures.append(f"{len(missing_serial)} configurations lack a matched direct-serial record")
    if duplicate_serial:
        failures.append(f"{len(duplicate_serial)} configurations have duplicate direct-serial records")

    controls = {
        "serial_bypass_enabled": parameter_value(records, "acg.serial_bypass_enabled"),
        "regime_change_enabled": parameter_value(records, "acg.regime_change_enabled"),
        "consensus_divergence": parameter_value(records, "consensus_divergence"),
    }
    lines = [
        "ConflictLab cross-strategy baseline smoke",
        "",
        "Cross-strategy normalization: matched direct-Serial actual block wall for the same seed and parameters.",
        "The per-record paired serial reference is retained separately as an internal timing diagnostic.",
        (
            "controls: "
            f"serial_bypass_enabled={controls['serial_bypass_enabled']} "
            f"regime_change_enabled={controls['regime_change_enabled']} "
            f"consensus_divergence={controls['consensus_divergence']}"
        ),
        "",
        "Overall (matched range is min/median/max):",
        "mode n correct matched-serial-range phase-median paired-seq-median post-ms replay conflicts replay-deps fallback",
    ]

    for mode in MODES:
        rows = groups.get(mode, [])
        if not rows:
            failures.append(f"missing mode {mode}")
            continue
        correct = sum(r["correctness"].get("serial_equivalent") is True for r in rows)
        if correct != len(rows):
            failures.append(f"{mode}: serial equivalence {correct}/{len(rows)}")
        matched_values = [matched_serial_speedup(r, serial_index) for r in rows]
        if any(value is None for value in matched_values):
            failures.append(f"{mode}: missing matched direct-serial normalization")
        phase = median(
            safe_ratio(
                r["consensus"].get("serial_validation_latency_nanos"),
                r["consensus"].get("bottleneck_nanos"),
            )
            for r in rows
        )
        paired_sequential = median(
            safe_ratio(
                r["pipeline_timing"].get("serial_reference_execution_nanos"),
                r["pipeline_timing"].get("total_adaptive_block_nanos"),
            )
            for r in rows
        )
        post = median(r["consensus"].get("post_consensus_nanos") for r in rows)
        replay = median(r["execution"].get("replayed_transactions", 0) for r in rows)
        conflicts = median((r.get("strategy") or {}).get("discovered_conflicts") for r in rows)
        replay_dependencies = median(
            (r.get("strategy") or {}).get("replay_dependencies")
            for r in rows
            if r.get("strategy") is not None
        )
        fallback = median(
            (
                (r.get("strategy") or {}).get("forward_conflict_fallbacks", 0)
                + (r.get("strategy") or {}).get("access_set_mismatch_fallbacks", 0)
            )
            for r in rows
            if r.get("strategy") is not None
        )
        lines.append(
            f"{mode:18s} {len(rows):2d} {correct:2d}/{len(rows):2d} "
            f"{fmt_range(matched_values):>20s} {fmt_x(phase):>11s} {fmt_x(paired_sequential):>17s} "
            f"{fmt_ms(post):>8s} {replay if replay is not None else 'n/a':>6} "
            f"{conflicts if conflicts is not None else 'n/a':>9} "
            f"{replay_dependencies if replay_dependencies is not None else 'n/a':>11} "
            f"{fallback if fallback is not None else 'n/a':>8}"
        )

    contentions = sorted(
        {
            str(record.get("metadata", {}).get("parameters", {}).get("contention", "n/a"))
            for record in records
        }
    )
    lines.extend(
        [
            "",
            "By contention (matched range is min/median/max):",
            "contention mode n matched-serial-range post-ms fallback",
        ]
    )
    for contention in contentions:
        for mode in MODES:
            rows = [
                row
                for row in groups.get(mode, [])
                if str(row.get("metadata", {}).get("parameters", {}).get("contention", "n/a"))
                == contention
            ]
            if not rows:
                continue
            matched_values = [matched_serial_speedup(r, serial_index) for r in rows]
            post = median(r["consensus"].get("post_consensus_nanos") for r in rows)
            fallback = median(
                (
                    (r.get("strategy") or {}).get("forward_conflict_fallbacks", 0)
                    + (r.get("strategy") or {}).get("access_set_mismatch_fallbacks", 0)
                )
                for r in rows
                if r.get("strategy") is not None
            )
            lines.append(
                f"{contention:10s} {mode:18s} {len(rows):2d} "
                f"{fmt_range(matched_values):>20s} {fmt_ms(post):>8s} "
                f"{fallback if fallback is not None else 'n/a':>8}"
            )

    lines.append("")
    if failures:
        lines.append("FAIL: " + "; ".join(failures))
    else:
        lines.append(
            f"PASS: {len(records)} records; every strategy is serial-equivalent and has an exact matched direct-Serial control."
        )
    lines.append(
        "NOTE: this 3-seed synthetic matrix is a baseline smoke/ablation, not publication-scale evidence."
    )
    report = "\n".join(lines) + "\n"
    if args.output:
        Path(args.output).write_text(report)
    if args.matched_output:
        write_matched_csv(Path(args.matched_output), records, serial_index)
    print(report, end="")
    raise SystemExit(1 if failures else 0)


if __name__ == "__main__":
    main()
