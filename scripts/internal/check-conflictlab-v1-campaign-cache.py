#!/usr/bin/env python3
"""Validate whether a completed ConflictLab 1.0 campaign can be safely reused."""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def load_json(path: Path):
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def identity(value: dict) -> tuple:
    metadata = value.get("metadata", value)
    return (
        metadata.get("workload"),
        metadata.get("mode"),
        metadata.get("run_index"),
        metadata.get("seed"),
        metadata.get("workers"),
        tuple(sorted(metadata.get("parameters", {}).items())),
    )


def check(campaign_dir: Path, expected_manifest: Path) -> tuple[bool, str]:
    manifest_path = campaign_dir / "manifest.json"
    acceptance_path = campaign_dir / "acceptance.json"
    records_path = campaign_dir / "records.jsonl"
    for path in (manifest_path, acceptance_path, records_path):
        if not path.is_file() or path.stat().st_size == 0:
            return False, f"missing {path.name}"

    try:
        expected = load_json(expected_manifest)
        cached = load_json(manifest_path)
        acceptance = load_json(acceptance_path)
    except (OSError, json.JSONDecodeError) as error:
        return False, f"unreadable campaign metadata: {error}"

    if cached != expected:
        return False, "cached manifest differs from the current grid expansion"

    runs = expected.get("runs", [])
    expected_count = len(runs)
    experiment_id = expected.get("experiment_id")
    if acceptance.get("experiment_id") != experiment_id:
        return False, "acceptance experiment_id differs from the current manifest"
    if acceptance.get("status") != "accepted":
        return False, f"acceptance status is {acceptance.get('status')!r}, not 'accepted'"
    for key in ("expected_runs", "observed_runs", "accepted_runs"):
        if acceptance.get(key) != expected_count:
            return False, f"acceptance {key}={acceptance.get(key)!r}, expected {expected_count}"
    for key in (
        "performance_regressions",
        "incomplete_runs",
        "configuration_errors",
        "correctness_failures",
    ):
        if acceptance.get(key) != 0:
            return False, f"acceptance {key}={acceptance.get(key)!r}"

    records = []
    try:
        with records_path.open("r", encoding="utf-8") as handle:
            for line_number, line in enumerate(handle, start=1):
                if not line.strip():
                    continue
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError as error:
                    return False, f"records.jsonl:{line_number}: {error}"
    except OSError as error:
        return False, f"cannot read records.jsonl: {error}"

    if len(records) != expected_count:
        return False, f"records.jsonl has {len(records)} records, expected {expected_count}"
    if any(record.get("metadata", {}).get("experiment_id") != experiment_id for record in records):
        return False, "records.jsonl contains a different experiment_id"

    expected_identities = {identity(run) for run in runs}
    observed_identities = {identity(record) for record in records}
    if len(expected_identities) != expected_count:
        return False, "current manifest contains duplicate run identities"
    if len(observed_identities) != expected_count:
        return False, "records.jsonl contains duplicate run identities"
    if observed_identities != expected_identities:
        return False, "records.jsonl run identities differ from the current manifest"

    return True, f"accepted {expected_count}-run campaign matches the current manifest"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("campaign_dir", type=Path)
    parser.add_argument("expected_manifest", type=Path)
    args = parser.parse_args()
    reusable, reason = check(args.campaign_dir, args.expected_manifest)
    print(reason)
    return 0 if reusable else 1


if __name__ == "__main__":
    raise SystemExit(main())
