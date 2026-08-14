#!/usr/bin/env python3
"""Require canonical state equivalence between reusable and recycled VM lifecycle runs."""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path


def lifecycle_key(record: dict) -> tuple:
    metadata = record["metadata"]
    params = dict(metadata.get("parameters", {}))
    params.pop("vm_instance_lifecycle", None)
    return (
        metadata.get("experiment_id"),
        metadata.get("mode"),
        metadata.get("seed"),
        metadata.get("workers"),
        tuple(sorted(params.items())),
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    args = parser.parse_args()

    records = [
        json.loads(line)
        for line in args.records.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    groups: dict[tuple, dict[str, dict]] = defaultdict(dict)
    for record in records:
        if record["metadata"].get("experiment_id") != "conflictlab-phase4-vm-lifecycle":
            continue
        lifecycle = record["metadata"].get("parameters", {}).get("vm_instance_lifecycle")
        if lifecycle not in {"reuse", "recycle"}:
            raise SystemExit(f"unexpected VM lifecycle {lifecycle!r}")
        groups[lifecycle_key(record)][lifecycle] = record

    if not groups:
        raise SystemExit("no Phase 4 VM lifecycle records found")

    for key, pair in groups.items():
        if set(pair) != {"reuse", "recycle"}:
            raise SystemExit(f"missing reuse/recycle pair for {key}: got {sorted(pair)}")
        reused = pair["reuse"]
        recycled = pair["recycle"]
        for lifecycle, record in pair.items():
            if record.get("correctness", {}).get("serial_equivalent") is not True:
                raise SystemExit(f"{lifecycle} run is not serial-equivalent for {key}")
        reuse_digest = reused["correctness"].get("canonical_state_digest")
        recycle_digest = recycled["correctness"].get("canonical_state_digest")
        if reuse_digest != recycle_digest:
            raise SystemExit(
                f"reuse/recycle canonical state mismatch for {key}: "
                f"reuse={reuse_digest} recycle={recycle_digest}"
            )

    print(f"PASS: reuse/recycle canonical state matches for {len(groups)} paired VM runs")


if __name__ == "__main__":
    main()
