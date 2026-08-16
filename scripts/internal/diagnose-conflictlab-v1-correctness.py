#!/usr/bin/env python3
"""Diagnose ConflictLab 1.0 correctness failures from cached schema-3 records.

The script is intentionally read-only with respect to campaign caches. It writes derived reports and
small filtered manifests under <output>/correctness-diagnostics so failing run identities can be
reproduced without rerunning accepted campaigns.
"""
from __future__ import annotations

import argparse
import csv
import json
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable

from conflictlab_v1_miss_policy import (
    UNEXPECTED_INPUT_RESOLVED,
    candidate_misses,
    classify_candidate_miss,
)

DIAGNOSTIC_DIRNAME = "correctness-diagnostics"


def load_json(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    with path.open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: {error}") from error
            record["__source_line"] = line_number
            records.append(record)
    return records


def metadata(record: dict[str, Any]) -> dict[str, Any]:
    return record.get("metadata", {})


def params(record: dict[str, Any]) -> dict[str, Any]:
    value = metadata(record).get("parameters", {})
    return value if isinstance(value, dict) else {}


def experiment_id(record: dict[str, Any]) -> str:
    return str(metadata(record).get("experiment_id", ""))


def serial_equivalent(record: dict[str, Any]) -> bool | None:
    return record.get("correctness", {}).get("serial_equivalent")


def run_identity(value: dict[str, Any]) -> tuple[Any, ...]:
    meta = value.get("metadata", value)
    parameters = meta.get("parameters", {})
    return (
        meta.get("workload"),
        meta.get("mode"),
        meta.get("run_index"),
        meta.get("seed"),
        meta.get("workers"),
        tuple(sorted(parameters.items())),
    )


def campaign_slug(exp_id: str) -> str:
    prefix = "conflictlab-v1-"
    return exp_id[len(prefix) :] if exp_id.startswith(prefix) else exp_id


def get_path(record: dict[str, Any], *path: str, default: Any = None) -> Any:
    current: Any = record
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return default
        current = current[key]
    return current


def as_int(value: Any, default: int = 0) -> int:
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def compact_record_row(record: dict[str, Any]) -> dict[str, Any]:
    meta = metadata(record)
    p = params(record)
    correctness = record.get("correctness", {})
    return {
        "source_line": record.get("__source_line"),
        "experiment_id": meta.get("experiment_id"),
        "campaign": campaign_slug(str(meta.get("experiment_id", ""))),
        "run_index": meta.get("run_index"),
        "mode": meta.get("mode"),
        "seed": meta.get("seed"),
        "workers": meta.get("workers"),
        "serial_equivalent": correctness.get("serial_equivalent"),
        "canonical_state_digest": correctness.get("canonical_state_digest"),
        "serial_reference_digest": correctness.get("serial_reference_digest"),
        "operation_mix": p.get("operation_mix"),
        "transactions": p.get("transactions"),
        "block_size": p.get("sim.block_size"),
        "accounts": p.get("accounts"),
        "contention": p.get("contention"),
        "hot_account_probability_bps": p.get("hot_account_probability_bps"),
        "warmup_blocks": p.get("warmup_blocks"),
        "postchange_warmup_blocks": p.get("postchange_warmup_blocks"),
        "warmup_hot_account_probability_bps": p.get("warmup_hot_account_probability_bps"),
        "prediction_quality": p.get("prediction_quality"),
        "prediction_buckets": p.get("prediction_buckets"),
        "serial_bypass_enabled": p.get("acg.serial_bypass_enabled"),
        "candidate_misses": candidate_misses(record),
        "candidate_miss_class": classify_candidate_miss(record) or "",
        "failed_preexecution_receipts": get_path(
            record, "consensus", "failed_preexecution_receipts", default=0
        ),
        "reused_receipts": get_path(record, "consensus", "reused_receipts", default=0),
        "replayed_transactions": get_path(
            record, "consensus", "replayed_transactions", default=0
        ),
        "missing_predictions": get_path(record, "consensus", "missing_predictions", default=0),
        "invalidated_predictions": get_path(
            record, "consensus", "invalidated_predictions", default=0
        ),
        "candidate_miss_history_relationships": get_path(
            record, "adaptive_state", "candidate_miss_history_relationships", default=0
        ),
        "runtime_fallback_relationships": get_path(
            record, "adaptive_state", "runtime_fallback_relationships", default=0
        ),
        "phase_bottleneck_speedup": get_path(
            record, "derived", "phase_bottleneck_speedup", default=""
        ),
    }


def write_csv(path: Path, rows: Iterable[dict[str, Any]], fieldnames: list[str] | None = None) -> None:
    rows = list(rows)
    path.parent.mkdir(parents=True, exist_ok=True)
    if fieldnames is None:
        fieldnames = list(rows[0].keys()) if rows else []
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fieldnames)
        writer.writeheader()
        for row in rows:
            writer.writerow({key: row.get(key, "") for key in fieldnames})


def discover_campaign_manifests(out: Path) -> dict[str, tuple[Path, dict[str, Any]]]:
    found: dict[str, tuple[Path, dict[str, Any]]] = {}
    for manifest_path in sorted(out.glob("*/manifest.json")):
        try:
            manifest = load_json(manifest_path)
        except (OSError, json.JSONDecodeError):
            continue
        exp_id = manifest.get("experiment_id")
        if isinstance(exp_id, str) and exp_id:
            found[exp_id] = (manifest_path, manifest)
    return found


def write_filtered_manifest(
    diagnostic_dir: Path,
    exp_id: str,
    manifest_path: Path,
    manifest: dict[str, Any],
    target_records: list[dict[str, Any]],
    suffix: str,
) -> Path:
    target_ids = {run_identity(record) for record in target_records}
    runs = [run for run in manifest.get("runs", []) if run_identity(run) in target_ids]
    missing = target_ids - {run_identity(run) for run in runs}
    if missing:
        raise ValueError(
            f"{exp_id}: {len(missing)} diagnostic record identities are absent from {manifest_path}"
        )
    filtered = dict(manifest)
    filtered["runs"] = runs
    target = diagnostic_dir / "manifests" / f"{campaign_slug(exp_id)}-{suffix}.manifest.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(json.dumps(filtered, indent=2) + "\n", encoding="utf-8")
    return target


def parameter_discrimination(records: list[dict[str, Any]], bad_ids: set[tuple[Any, ...]]) -> list[dict[str, Any]]:
    by_campaign: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for record in records:
        by_campaign[experiment_id(record)].append(record)

    rows: list[dict[str, Any]] = []
    for exp_id, campaign_records in sorted(by_campaign.items()):
        campaign_bad = [record for record in campaign_records if run_identity(record) in bad_ids]
        if not campaign_bad:
            continue
        keys = sorted({key for record in campaign_records for key in params(record)})
        dimensions = ["mode", "seed", *keys]
        for dimension in dimensions:
            totals: Counter[str] = Counter()
            failures: Counter[str] = Counter()
            for record in campaign_records:
                if dimension == "mode":
                    value = metadata(record).get("mode")
                elif dimension == "seed":
                    value = metadata(record).get("seed")
                else:
                    value = params(record).get(dimension)
                label = "<missing>" if value is None else str(value)
                totals[label] += 1
                if run_identity(record) in bad_ids:
                    failures[label] += 1
            if len(totals) <= 1:
                continue
            for value, total in sorted(totals.items()):
                failed = failures[value]
                rows.append(
                    {
                        "experiment_id": exp_id,
                        "dimension": dimension,
                        "value": value,
                        "failed": failed,
                        "total": total,
                        "failure_rate": f"{failed / total:.6f}",
                    }
                )
    return rows


def campaign_status_rows(out: Path, manifests: dict[str, tuple[Path, dict[str, Any]]]) -> list[dict[str, Any]]:
    rows = []
    for exp_id, (manifest_path, manifest) in sorted(manifests.items()):
        campaign_dir = manifest_path.parent
        acceptance_path = campaign_dir / "acceptance.json"
        acceptance: dict[str, Any] = {}
        if acceptance_path.is_file():
            try:
                acceptance = load_json(acceptance_path)
            except (OSError, json.JSONDecodeError):
                pass
        rows.append(
            {
                "experiment_id": exp_id,
                "campaign": campaign_dir.name,
                "manifest_runs": len(manifest.get("runs", [])),
                "acceptance_status": acceptance.get("status", "missing"),
                "accepted_runs": acceptance.get("accepted_runs", ""),
                "performance_regressions": acceptance.get("performance_regressions", ""),
                "incomplete_runs": acceptance.get("incomplete_runs", ""),
                "configuration_errors": acceptance.get("configuration_errors", ""),
                "correctness_failures": acceptance.get("correctness_failures", ""),
            }
        )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("output_dir", type=Path, help="ConflictLab V1 output directory")
    parser.add_argument(
        "--diagnostic-dir",
        type=Path,
        default=None,
        help="derived output directory (default: <output>/correctness-diagnostics)",
    )
    args = parser.parse_args()

    out = args.output_dir.resolve()
    records_path = out / "records.jsonl"
    if not records_path.is_file():
        raise SystemExit(f"missing combined records: {records_path}")
    diagnostic_dir = (args.diagnostic_dir or (out / DIAGNOSTIC_DIRNAME)).resolve()
    diagnostic_dir.mkdir(parents=True, exist_ok=True)

    records = load_jsonl(records_path)
    incorrect = [record for record in records if serial_equivalent(record) is False]
    unexpected_miss = [
        record
        for record in records
        if candidate_misses(record) > 0
        and classify_candidate_miss(record) == UNEXPECTED_INPUT_RESOLVED
    ]
    bad_ids = {run_identity(record) for record in incorrect}

    write_csv(diagnostic_dir / "incorrect-records.csv", map(compact_record_row, incorrect))
    write_csv(
        diagnostic_dir / "unexpected-input-resolved-candidate-misses.csv",
        map(compact_record_row, unexpected_miss),
    )
    discrimination = parameter_discrimination(records, bad_ids)
    write_csv(diagnostic_dir / "failure-discrimination.csv", discrimination)

    manifests = discover_campaign_manifests(out)
    statuses = campaign_status_rows(out, manifests)
    write_csv(diagnostic_dir / "campaign-status.csv", statuses)

    rerun_manifests: list[dict[str, Any]] = []
    for exp_id in sorted({experiment_id(record) for record in incorrect}):
        if exp_id not in manifests:
            continue
        manifest_path, manifest = manifests[exp_id]
        target = [record for record in incorrect if experiment_id(record) == exp_id]
        filtered = write_filtered_manifest(
            diagnostic_dir, exp_id, manifest_path, manifest, target, "incorrect"
        )
        rerun_manifests.append(
            {
                "reason": "serial-non-equivalent",
                "experiment_id": exp_id,
                "campaign": campaign_slug(exp_id),
                "runs": len(target),
                "manifest": str(filtered),
            }
        )

    for exp_id in sorted({experiment_id(record) for record in unexpected_miss}):
        if exp_id not in manifests:
            continue
        manifest_path, manifest = manifests[exp_id]
        target = [record for record in unexpected_miss if experiment_id(record) == exp_id]
        filtered = write_filtered_manifest(
            diagnostic_dir, exp_id, manifest_path, manifest, target, "unexpected-miss"
        )
        rerun_manifests.append(
            {
                "reason": "unexpected-input-resolved-candidate-miss",
                "experiment_id": exp_id,
                "campaign": campaign_slug(exp_id),
                "runs": len(target),
                "manifest": str(filtered),
            }
        )

    (diagnostic_dir / "rerun-plan.json").write_text(
        json.dumps(rerun_manifests, indent=2) + "\n", encoding="utf-8"
    )

    incorrect_by_campaign = Counter(experiment_id(record) for record in incorrect)
    miss_by_campaign = Counter(experiment_id(record) for record in unexpected_miss)
    total_misses = sum(candidate_misses(record) for record in unexpected_miss)

    lines = [
        "# ConflictLab V1 correctness diagnostics",
        "",
        f"records={len(records)}",
        f"serial_non_equivalent={len(incorrect)}",
        f"unexpected_input_resolved_miss_records={len(unexpected_miss)}",
        f"unexpected_input_resolved_candidate_misses={total_misses}",
        "",
        "## Non-serial-equivalent records by campaign",
    ]
    if incorrect_by_campaign:
        for exp_id, count in sorted(incorrect_by_campaign.items()):
            lines.append(f"- {exp_id}: {count}")
    else:
        lines.append("- none")
    lines.extend(["", "## Unexpected input-resolved candidate misses by campaign"])
    if miss_by_campaign:
        for exp_id, count in sorted(miss_by_campaign.items()):
            misses = sum(
                candidate_misses(record)
                for record in unexpected_miss
                if experiment_id(record) == exp_id
            )
            lines.append(f"- {exp_id}: {count} records / {misses} misses")
    else:
        lines.append("- none")

    lines.extend(["", "## Highest failure-rate discriminators"])
    ranked = sorted(
        (row for row in discrimination if row["failed"]),
        key=lambda row: (-float(row["failure_rate"]), -int(row["failed"]), row["dimension"]),
    )[:40]
    for row in ranked:
        lines.append(
            f"- {row['experiment_id']} {row['dimension']}={row['value']}: "
            f"{row['failed']}/{row['total']} ({float(row['failure_rate']) * 100:.1f}%)"
        )
    if not ranked:
        lines.append("- none")

    lines.extend(
        [
            "",
            "## Generated targeted manifests",
        ]
    )
    for item in rerun_manifests:
        lines.append(
            f"- {item['reason']}: {item['campaign']} {item['runs']} runs -> {item['manifest']}"
        )
    if not rerun_manifests:
        lines.append("- none")

    summary = "\n".join(lines) + "\n"
    (diagnostic_dir / "summary.txt").write_text(summary, encoding="utf-8")
    print(summary, end="")
    print(f"diagnostics_dir={diagnostic_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
