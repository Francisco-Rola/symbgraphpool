#!/usr/bin/env python3
"""Summarize focused validation benchmarks for the four post-V1 ConflictLab fixes."""

from __future__ import annotations

import argparse
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path


EXPECTED_RECORDS = {
    "reorder": 6,
    "regime": 100,
    "miss": 16,
    "cost": 24,
}


def load(path: Path) -> list[dict]:
    with path.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def med(values) -> float:
    values = list(values)
    return statistics.median(values) if values else 0.0


def percentile(values, quantile: float) -> float:
    ordered = sorted(float(value) for value in values)
    if not ordered:
        return 0.0
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def ratio(n, d) -> float:
    return float(n) / float(d) if d else 0.0


def seq_speedup(r: dict) -> float:
    milli = r.get("pipeline_timing", {}).get("end_to_end_speedup_milli")
    if isinstance(milli, (int, float)):
        return float(milli) / 1000.0
    return ratio(
        r.get("parallelism", {}).get("serial_equivalent_work_nanos", 0),
        r.get("pipeline_timing", {}).get("total_adaptive_block_nanos", 0),
    )


def phase_speedup(r: dict) -> float:
    milli = r.get("consensus", {}).get("throughput_speedup_milli", 0)
    return float(milli) / 1000.0 if milli else 0.0


def precision(r: dict) -> float:
    f = r.get("feedback", {})
    pos, neg = f.get("positive_observations", 0), f.get("negative_observations", 0)
    return ratio(pos, pos + neg)


def recall(r: dict) -> float:
    f = r.get("feedback", {})
    pos, misses = f.get("positive_observations", 0), f.get("candidate_misses", 0)
    return ratio(pos, pos + misses)


def transition_name(params: dict) -> str:
    hot = params.get("hot_account_probability_bps")
    warm_hot = params.get("warmup_hot_account_probability_bps", hot)
    work = params.get("work_iterations")
    warm_work = params.get("warmup_work_iterations", work)
    if hot != warm_hot:
        return f"contention {warm_hot}->{hot}"
    if work != warm_work:
        return f"work {warm_work}->{work}"
    return "unchanged"


def direct_bypass_violations(record: dict) -> list[str]:
    """Return structural violations for a non-buffered direct-serial bypass record."""
    if not record.get("planning", {}).get("serial_bypassed"):
        return []
    params = record.get("metadata", {}).get("parameters", {})
    if params.get("acg.serial_bypass_buffered_preexecution") != "false":
        return []

    execution = record.get("execution", {})
    consensus = record.get("consensus", {})
    pipeline = record.get("pipeline_timing", {})
    feedback_timing = record.get("feedback_timing", {})
    expected_transactions = execution.get("transactions", consensus.get("decided_transactions", 0))

    checks = {
        "workers!=1": execution.get("workers") == 1,
        "speculative_results!=0": execution.get("speculative_results", 0) == 0,
        "predicted_transactions!=0": execution.get("predicted_transactions", 0) == 0,
        "prepared_receipts!=0": consensus.get("prepared_receipts", 0) == 0,
        "successful_preexecution_receipts!=0": consensus.get("successful_preexecution_receipts", 0) == 0,
        "failed_preexecution_receipts!=0": consensus.get("failed_preexecution_receipts", 0) == 0,
        "preexecution_nanos!=0": pipeline.get("preexecution_nanos", 0) == 0,
        "preexecution_executor_total_nanos!=0": execution.get("preexecution_executor_total_nanos", 0) == 0,
        "preexecution_worker_wall_nanos!=0": execution.get("preexecution_worker_wall_nanos", 0) == 0,
        "feedback_timing!=0": feedback_timing.get("total_nanos", 0) == 0,
        "reused_results!=0": execution.get("reused_results", 0) == 0,
        "invalidated_results!=0": execution.get("invalidated_results", 0) == 0,
        "replayed_transactions!=0": execution.get("replayed_transactions", 0) == 0,
        "canonical_transactions!=transactions": execution.get("canonical_transactions", 0) == expected_transactions,
    }
    return [label for label, ok in checks.items() if not ok]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="directory containing reorder/regime/miss/cost subdirectories")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    paths = {name: args.root / name / "records.jsonl" for name in EXPECTED_RECORDS}
    records: dict[str, list[dict]] = {}
    failures: list[str] = []
    for name, path in paths.items():
        try:
            records[name] = load(path)
        except (OSError, json.JSONDecodeError) as exc:
            records[name] = []
            failures.append(f"{name}: could not load {path}: {exc}")
            continue
        if len(records[name]) != EXPECTED_RECORDS[name]:
            failures.append(
                f"{name}: expected {EXPECTED_RECORDS[name]} records, observed {len(records[name])}"
            )
        bad = [
            r.get("metadata", {}).get("run_index")
            for r in records[name]
            if not r.get("correctness", {}).get("serial_equivalent")
        ]
        if bad:
            failures.append(f"{name}: non-serial-equivalent runs: {bad[:8]}")

    lines: list[str] = ["ConflictLab focused four-fix validation", ""]

    # 1. Reorder/read-set validity.
    rr = records["reorder"]
    reorder_failures: list[str] = []
    if rr:
        if any(r["execution"]["discarded_predictions"] != 0 for r in rr):
            reorder_failures.append("moved receipts were discarded before read-set validation")
        if any(r["execution"]["matched_transactions"] != r["consensus"]["shared_transactions"] for r in rr):
            reorder_failures.append("not all shared transactions were matched by ID/request")
        if any(r["execution"]["reused_results"] != r["consensus"]["shared_transactions"] for r in rr):
            reorder_failures.append("independent moved receipts did not all survive read-set validation")
        if any(r["execution"]["invalidated_results"] != 0 for r in rr):
            reorder_failures.append("independent moved receipts were unexpectedly stale")
        moved = [
            r["consensus"]["shared_transactions"] - r["consensus"]["same_position_transactions"]
            for r in rr
        ]
        lines += [
            "1) Reordered receipt reuse (concrete read-set is the validity boundary)",
            f"records={len(rr)} discarded={sum(r['execution']['discarded_predictions'] for r in rr)} "
            f"median_moved={med(moved):.1f} median_reused={med(r['execution']['reused_results'] for r in rr):.1f} "
            f"median_invalidated={med(r['execution']['invalidated_results'] for r in rr):.1f} "
            f"median_replayed={med(r['execution']['replayed_transactions'] for r in rr):.1f}",
        ]
    else:
        lines += ["1) Reordered receipt reuse (concrete read-set is the validity boundary)", "records=0"]
        reorder_failures.append("records unavailable")
    if reorder_failures:
        lines.append("FAIL: " + "; ".join(reorder_failures))
        failures.extend(f"reorder: {failure}" for failure in reorder_failures)
    else:
        lines.append("PASS: independent moved receipts are matched by transaction ID/request and reused after concrete read-set validation.")
    lines.append("")

    # 2. Regime fail-safe.
    rg = records["regime"]
    grouped: dict[tuple[str, str, int], list[dict]] = defaultdict(list)
    for r in rg:
        p = r["metadata"]["parameters"]
        grouped[(r["metadata"]["mode"], transition_name(p), int(p["postchange_warmup_blocks"]))].append(r)
    lines += [
        "2) Regime-change fail-safe",
        "mode transition depth bypass seq-speedup phase-speedup projected-speedup proj-error",
    ]
    probation_failures: list[str] = []
    missing_probe_groups: list[str] = []
    failed_rearm_pairs: list[str] = []
    structural_failures: list[str] = []
    bypass_ratios: list[float] = []

    for key, xs in sorted(grouped.items()):
        mode, transition, depth = key
        bypass_rate = 100.0 * sum(bool(r["planning"]["serial_bypassed"]) for r in xs) / len(xs)
        projected = med((r["planning"].get("serial_bypass_projected_speedup_milli") or 0) / 1000.0 for r in xs)
        seq = med(seq_speedup(r) for r in xs)
        phase = med(phase_speedup(r) for r in xs)
        projection_error = projected - phase if projected else 0.0
        lines.append(
            f"{mode:16s} {transition:28s} d={depth:<2d} bypass={bypass_rate:5.1f}% "
            f"seq={seq:.2f}x phase={phase:.2f}x proj={projected:.2f}x err={projection_error:+.2f}x"
        )

        label = f"{mode}/{transition}/d={depth}"
        if depth in (1, 2) and bypass_rate < 100.0:
            probation_failures.append(label)
        if depth == 3 and bypass_rate != 0.0:
            missing_probe_groups.append(label)

        for r in xs:
            if r["planning"]["serial_bypassed"]:
                bypass_ratios.append(seq_speedup(r))
                violations = direct_bypass_violations(r)
                if violations:
                    structural_failures.append(
                        f"{label}/seed={r['metadata']['seed']}: {','.join(violations)}"
                    )

    regime_by_identity = {}
    for r in rg:
        p = r["metadata"]["parameters"]
        regime_by_identity[(
            r["metadata"]["mode"],
            transition_name(p),
            r["metadata"]["seed"],
            int(p["postchange_warmup_blocks"]),
        )] = r
    for (mode, transition, seed, depth), probe in regime_by_identity.items():
        if depth != 3 or phase_speedup(probe) >= 1.10:
            continue
        followup = regime_by_identity.get((mode, transition, seed, 4))
        if followup is None or not followup["planning"]["serial_bypassed"]:
            failed_rearm_pairs.append(f"{mode}/{transition}/seed={seed}")

    if bypass_ratios:
        lines += [
            "direct-bypass timing diagnostic (serial-reference/adaptive-wall; timing-only, not a correctness gate):",
            f"n={len(bypass_ratios)} min={min(bypass_ratios):.3f}x p25={percentile(bypass_ratios, 0.25):.3f}x "
            f"median={med(bypass_ratios):.3f}x p75={percentile(bypass_ratios, 0.75):.3f}x max={max(bypass_ratios):.3f}x",
        ]

    regime_failures: list[str] = []
    if probation_failures:
        regime_failures.append("first two changed-regime blocks were not all direct-serial bypasses: " + ", ".join(probation_failures))
    if structural_failures:
        regime_failures.append("direct bypass violated structural serial-path invariants: " + "; ".join(structural_failures[:8]))
    if missing_probe_groups:
        regime_failures.append("bounded counterfactual probe did not run after probation: " + ", ".join(missing_probe_groups))
    if failed_rearm_pairs:
        regime_failures.append("sub-threshold counterfactual probe did not re-arm probation: " + ", ".join(failed_rearm_pairs))
    if not rg:
        regime_failures.append("records unavailable")

    if regime_failures:
        lines.append("FAIL: " + " | ".join(regime_failures))
        failures.extend(f"regime: {failure}" for failure in regime_failures)
    else:
        lines += [
            "PASS: d=1/d=2 are structurally direct serial bypasses, d=3 is the bounded adaptive probe, and sub-threshold probes re-arm d=4 probation.",
            "INFO: bypass-vs-serial timing is diagnostic only because both paths are independently timed and short blocks show host/VM scheduling noise.",
        ]
    lines.append("")

    # 3. Miss-history precision recovery.
    mr = records["miss"]
    grouped_miss: dict[tuple[str, int], list[dict]] = defaultdict(list)
    for r in mr:
        p = r["metadata"]["parameters"]
        grouped_miss[(r["metadata"]["mode"], int(p["postchange_warmup_blocks"]))].append(r)
    lines += [
        "3) Transient hidden-key miss recovery",
        "mode depth misses precision recall active-miss-history",
    ]
    depth0_misses = 0
    recovered_groups = 0
    for (mode, depth), xs in sorted(grouped_miss.items()):
        misses = med(r["feedback"]["candidate_misses"] for r in xs)
        active = med(r.get("adaptive_state", {}).get("candidate_miss_history_relationships", 0) for r in xs)
        pr = med(precision(r) for r in xs)
        rc = med(recall(r) for r in xs)
        if depth == 0:
            depth0_misses += int(misses > 0)
        if depth >= 2 and misses == 0 and active == 0:
            recovered_groups += 1
        lines.append(f"{mode:16s} d={depth:<2d} misses={misses:8.1f} precision={pr:.3f} recall={rc:.3f} active={active:.1f}")
    miss_failures: list[str] = []
    if not mr:
        miss_failures.append("records unavailable")
    if mr and depth0_misses == 0:
        miss_failures.append("transient hidden-key block did not create a candidate miss")
    if mr and recovered_groups == 0:
        miss_failures.append("targeted verification did not retire any broad miss-history override by depth >=2")
    if miss_failures:
        lines.append("FAIL: " + "; ".join(miss_failures))
        failures.extend(f"miss: {failure}" for failure in miss_failures)
    else:
        lines.append("PASS: transient faults create safety history, then clean predicate-False verification can retire the broad override.")
    lines.append("")

    # 4. Cost-aware combined-wall objective.
    cr = records["cost"]
    grouped_cost: dict[tuple[str, str, str], list[dict]] = defaultdict(list)
    for r in cr:
        p = r["metadata"]["parameters"]
        grouped_cost[(p["contention"], p["acg.risk_budget"], r["metadata"]["mode"])].append(r)
    lines += [
        "4) Cost-aware combined-pipeline-wall objective",
        "contention risk mode seq-speedup phase-speedup adaptive-ms replay",
    ]
    for key, xs in sorted(grouped_cost.items()):
        contention, risk, mode = key
        lines.append(
            f"{contention:8s} risk={risk:>4s} {mode:16s} seq={med(seq_speedup(r) for r in xs):.2f}x "
            f"phase={med(phase_speedup(r) for r in xs):.2f}x "
            f"adaptive={med(r['pipeline_timing']['total_adaptive_block_nanos'] for r in xs)/1e6:.2f}ms "
            f"replay={med(r['execution']['replayed_transactions'] for r in xs):.1f}"
        )
    if cr:
        lines.append("INFO: this section is comparative rather than a pass/fail claim; the unit test enforces the new break-even objective, while these runs show its system-level tradeoff.")
    else:
        lines.append("FAIL: records unavailable")
        failures.append("cost: records unavailable")
    lines.append("")

    total_records = sum(len(v) for v in records.values())
    if failures:
        lines += [
            f"FAIL: focused validation completed with {len(failures)} issue(s) ({total_records} records loaded).",
            "Failures:",
        ]
        lines.extend(f"- {failure}" for failure in failures)
    else:
        lines.append(f"PASS: focused validation completed ({total_records} records, all serial-equivalent).")

    text = "\n".join(lines) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(text, end="")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
