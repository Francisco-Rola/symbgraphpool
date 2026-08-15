#!/usr/bin/env python3
"""Structural/correctness validator for the Phase 6 consensus-realism campaign."""

from __future__ import annotations

import json
import math
import statistics
import sys
from collections import Counter
from pathlib import Path
from typing import Any

EXPECTED = {
    "conflictlab-phase6-feature-state": 576,
    "conflictlab-phase6-consensus-cutoff": 96,
    "conflictlab-phase6-serial-preexecution-cutoff": 24,
    "conflictlab-phase6-consensus-divergence": 192,
    "conflictlab-phase6-policy-sensitivity": 48,
    "conflictlab-phase6-vm-lifecycle-sanity": 16,
}
ALLOWED_CUTOFF_MS = {250, 500, 1000}


def load(path: Path) -> list[dict[str, Any]]:
    rows = []
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise SystemExit(f"{path}:{line_number}: invalid JSON: {error}") from error
    return rows


def p(record: dict[str, Any], key: str, default: str = "-") -> str:
    return record["metadata"]["parameters"].get(key, default)


def ratio_milli(numerator: int, denominator: int) -> int:
    denominator = max(denominator, 1)
    return (numerator * 1000 + denominator // 2) // denominator


def divergence_count(total: int, percent: int) -> int:
    return 0 if total == 0 or percent == 0 else min(total, (total * percent + 99) // 100)


def median(values: list[float]) -> float:
    return statistics.median(values) if values else float("nan")


def validate(records: list[dict[str, Any]]) -> list[str]:
    failures: list[str] = []
    counts = Counter(r["metadata"]["experiment_id"] for r in records)
    if counts != EXPECTED:
        failures.append(f"experiment counts {dict(counts)} != {EXPECTED}")
    if len(records) != sum(EXPECTED.values()):
        failures.append(f"expected {sum(EXPECTED.values())} records, got {len(records)}")

    for record in records:
        run = record["metadata"]["run_index"]
        params = record["metadata"]["parameters"]
        execution = record["execution"]
        consensus = record.get("consensus", {})

        if record.get("schema_version") != 3:
            failures.append(f"run {run}: expected schema v3")
            continue
        if record["correctness"].get("serial_equivalent") is not True:
            failures.append(f"run {run}: serial equivalence failed")
        if record["feedback"].get("candidate_misses", 0) != 0:
            failures.append(f"run {run}: candidate_misses != 0")
        if any(key.startswith("acg.exploration_") for key in params):
            failures.append(f"run {run}: exploration parameters are not part of Phase 6")
        if params.get("execution_backend") != "wasm":
            failures.append(f"run {run}: Phase 6 must use real Wasm")
        lifecycle = params.get("vm_instance_lifecycle")
        lifecycle_campaign = record["metadata"]["experiment_id"] == "conflictlab-phase6-vm-lifecycle-sanity"
        if lifecycle_campaign:
            if lifecycle not in {"reuse", "recycle"}:
                failures.append(f"run {run}: lifecycle sanity run has invalid lifecycle {lifecycle!r}")
        elif lifecycle != "reuse":
            failures.append(f"run {run}: Phase 6 production campaigns must use VM reuse")
        if lifecycle == "reuse" and execution["contract"].get("wasm_instance_recycles", 0) != 0:
            failures.append(f"run {run}: reuse-mode execution recycled a Wasm instance")
        if lifecycle == "recycle" and execution["contract"].get("wasm_instance_recycles", 0) == 0:
            failures.append(f"run {run}: recycle lifecycle recorded no Wasm recycles")

        cutoff_ms = int(params.get("consensus_cutoff_ms", "0"))
        if cutoff_ms not in ALLOWED_CUTOFF_MS:
            failures.append(f"run {run}: unsupported cutoff {cutoff_ms}ms")
        if consensus.get("cutoff_nanos") != cutoff_ms * 1_000_000:
            failures.append(f"run {run}: consensus cutoff record disagrees with manifest")
        candidate = int(consensus.get("candidate_transactions", 0))
        decided = int(consensus.get("decided_transactions", 0))
        if candidate <= 0 or decided <= 0:
            failures.append(f"run {run}: missing candidate/decided block sizes")
        if execution.get("transactions") != decided:
            failures.append(f"run {run}: execution transaction count != decided block size")
        if execution.get("predicted_transactions") != candidate:
            failures.append(f"run {run}: predicted transaction count != candidate block size")
        prepared = int(consensus.get("prepared_receipts", 0))
        if prepared != execution.get("speculative_results"):
            failures.append(f"run {run}: prepared receipt count != speculative_results")
        if int(consensus.get("receipts_ready_by_cutoff", 0)) + int(
            consensus.get("receipts_completed_after_cutoff", 0)
        ) != prepared:
            failures.append(f"run {run}: cutoff receipt classification does not sum to prepared receipts")
        pre = int(consensus.get("pre_consensus_nanos", 0))
        overrun = int(consensus.get("pre_consensus_overrun_nanos", 0))
        post = int(consensus.get("post_consensus_nanos", 0))
        bottleneck = int(consensus.get("bottleneck_nanos", 0))
        if pre > cutoff_ms * 1_000_000:
            failures.append(f"run {run}: pre-consensus time exceeds cutoff")
        if post < overrun:
            failures.append(f"run {run}: post-consensus time is smaller than cutoff overrun")
        if bottleneck != max(pre, post):
            failures.append(f"run {run}: bottleneck != max(pre, post)")
        serial = consensus.get("serial_validation_latency_nanos")
        if serial is None or int(serial) <= 0:
            failures.append(f"run {run}: missing serial validation baseline")
        else:
            if consensus.get("validation_latency_speedup_milli") != ratio_milli(int(serial), post):
                failures.append(f"run {run}: validation speedup does not match timings")
            if consensus.get("throughput_speedup_milli") != ratio_milli(int(serial), bottleneck):
                failures.append(f"run {run}: throughput speedup does not match timings")

        if record["planning"].get("serial_bypassed"):
            if execution.get("workers") != 1 or execution.get("max_in_flight", 0) > 1:
                failures.append(f"run {run}: serial fallback was not one-worker serial pre-execution")
            if record["scheduling"].get("soft_dependencies", 0) != 0:
                failures.append(f"run {run}: serial fallback contains soft dependencies")
            expected_chain = max(candidate - 1, 0)
            if execution.get("dependency_count") != expected_chain:
                failures.append(f"run {run}: serial fallback dependency chain is not N-1")
            if execution.get("hard_dependency_count") != expected_chain:
                failures.append(f"run {run}: serial fallback hard dependency chain is not N-1")

        divergence = params.get("consensus_divergence", "identical")
        shared = int(consensus.get("shared_transactions", 0))
        same = int(consensus.get("same_position_transactions", 0))
        prefix = int(consensus.get("common_prefix_transactions", 0))
        if divergence == "identical":
            if (shared, same, prefix) != (candidate, candidate, candidate):
                failures.append(f"run {run}: identical decision reports divergence")
        elif divergence in {"tail-5pct", "tail-20pct"}:
            percent = 5 if divergence == "tail-5pct" else 20
            replaced = divergence_count(candidate, percent)
            expected_shared = candidate - replaced
            if (shared, same, prefix) != (expected_shared, expected_shared, expected_shared):
                failures.append(f"run {run}: tail replacement shape is inconsistent")
            if execution.get("missing_predictions", 0) < replaced:
                failures.append(f"run {run}: tail replacement did not create expected missing predictions")
        elif divergence in {"reorder-5pct", "reorder-20pct"}:
            percent = 5 if divergence == "reorder-5pct" else 20
            reordered = divergence_count(candidate, percent)
            expected_same = candidate - reordered + (reordered % 2 if reordered > 1 else reordered)
            expected_prefix = candidate - reordered if reordered > 1 else candidate
            if shared != candidate or same != expected_same or prefix != expected_prefix:
                failures.append(f"run {run}: reorder divergence shape is inconsistent")
        elif divergence == "tail-reorder-10pct":
            replaced = divergence_count(candidate, 10)
            expected_shared = candidate - replaced
            if (shared, same, prefix) != (expected_shared, expected_shared, expected_shared):
                failures.append(f"run {run}: combined divergence shape is inconsistent")
        else:
            failures.append(f"run {run}: unknown divergence mode {divergence!r}")

        if len(failures) >= 50:
            failures.append("stopping after 50 failures")
            break

    forced = [
        r for r in records
        if r["metadata"]["experiment_id"] == "conflictlab-phase6-serial-preexecution-cutoff"
    ]
    if not forced or not all(r["planning"].get("serial_bypassed") for r in forced):
        failures.append("forced serial-preexecution campaign did not bypass on every measured block")
    stressed = [
        r for r in forced
        if p(r, "complexity") == "high"
        and p(r, "consensus_cutoff_ms") == "250"
        and r["consensus"].get("cutoff_reached")
    ]
    if not stressed:
        failures.append("250ms high-complexity serial campaign never exercised the physical cutoff")

    divergence_rows = [
        r for r in records
        if r["metadata"]["experiment_id"] == "conflictlab-phase6-consensus-divergence"
        and p(r, "consensus_divergence") != "identical"
    ]
    if not divergence_rows or not any(r["execution"].get("missing_predictions", 0) > 0 for r in divergence_rows):
        failures.append("divergence campaign did not exercise missing predictions")
    if not any(
        p(r, "consensus_divergence").startswith("reorder")
        and r["consensus"].get("same_position_transactions", 0)
        < r["consensus"].get("shared_transactions", 0)
        for r in divergence_rows
    ):
        failures.append("divergence campaign did not exercise ordering perturbation")

    lifecycle_rows = [
        record
        for record in records
        if record["metadata"]["experiment_id"] == "conflictlab-phase6-vm-lifecycle-sanity"
    ]
    lifecycle_pairs: dict[tuple[Any, ...], list[dict[str, Any]]] = {}
    for record in lifecycle_rows:
        params = record["metadata"]["parameters"]
        key = (
            record["metadata"]["seed"],
            params.get("complexity"),
            params.get("complexity_mix"),
            params.get("transactions"),
            params.get("prediction_quality"),
            params.get("contention"),
        )
        lifecycle_pairs.setdefault(key, []).append(record)
    for key, pair in lifecycle_pairs.items():
        by_lifecycle = {p(record, "vm_instance_lifecycle"): record for record in pair}
        if set(by_lifecycle) != {"reuse", "recycle"}:
            failures.append(f"VM lifecycle pair {key}: expected one reuse and one recycle record")
            continue
        if (
            by_lifecycle["reuse"]["correctness"].get("canonical_state_digest")
            != by_lifecycle["recycle"]["correctness"].get("canonical_state_digest")
        ):
            failures.append(f"VM lifecycle pair {key}: canonical state digests differ")

    return failures


def report(records: list[dict[str, Any]]) -> None:
    print(
        f"Phase 6 structural checks: records={len(records)} "
        f"correct={sum(r['correctness'].get('serial_equivalent') is True for r in records)}/{len(records)} "
        f"bypassed={sum(bool(r['planning'].get('serial_bypassed')) for r in records)} "
        f"candidate_misses={sum(r['feedback'].get('candidate_misses', 0) for r in records)}"
    )
    for cutoff in (250, 500, 1000):
        rows = [r for r in records if p(r, "consensus_cutoff_ms") == str(cutoff)]
        if not rows:
            continue
        validation = [r["consensus"]["validation_latency_speedup_milli"] / 1000 for r in rows]
        throughput = [r["consensus"]["throughput_speedup_milli"] / 1000 for r in rows]
        reached = sum(bool(r["consensus"].get("cutoff_reached")) for r in rows)
        print(
            f"cutoff={cutoff}ms n={len(rows)} cutoff_reached={reached} "
            f"median_validation={median(validation):.2f}x median_throughput={median(throughput):.2f}x"
        )
    div = [r for r in records if p(r, "consensus_divergence") != "identical"]
    if div:
        reuse = [
            r["execution"].get("reused_results", 0) / max(1, r["consensus"].get("decided_transactions", 0))
            for r in div
        ]
        print(f"divergence n={len(div)} median decided-result reuse={100*median(reuse):.1f}%")
    for mode in ("static", "probability-only", "cost-aware"):
        rows = [r for r in records if r["metadata"]["mode"] == mode and not r["planning"].get("serial_bypassed")]
        if rows:
            th = [r["consensus"]["throughput_speedup_milli"] / 1000 for r in rows]
            val = [r["consensus"]["validation_latency_speedup_milli"] / 1000 for r in rows]
            replay = [r["execution"].get("replayed_transactions", 0) for r in rows]
            print(
                f"mode={mode:16s} n={len(rows)} median_validation={median(val):.2f}x "
                f"median_throughput={median(th):.2f}x median_replay={median(replay):.1f}"
            )


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} RECORDS.jsonl")
    records = load(Path(sys.argv[1]))
    failures = validate(records)
    report(records)
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print("PASS: Phase 6 consensus-realism structural/correctness validation")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
