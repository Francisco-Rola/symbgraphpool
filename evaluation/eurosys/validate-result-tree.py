#!/usr/bin/env python3
"""Cheap integrity scan for a completed/smoke EuroSys result tree."""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path


def walk_numbers(value, path=""):
    if isinstance(value, dict):
        for key, child in value.items():
            yield from walk_numbers(child, f"{path}.{key}" if path else key)
    elif isinstance(value, list):
        for i, child in enumerate(value):
            yield from walk_numbers(child, f"{path}[{i}]")
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        yield path, value


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("result_root", type=Path)
    args = ap.parse_args()

    files = sorted(args.result_root.rglob("records.jsonl"))
    if not files:
        raise SystemExit(f"no records.jsonl files found below {args.result_root}")

    records = 0
    parse_errors = 0
    serial_failures = 0
    nonfinite = 0
    negative_times = 0

    for path in files:
        with path.open(encoding="utf-8") as handle:
            for lineno, line in enumerate(handle, 1):
                if not line.strip():
                    continue
                records += 1
                try:
                    row = json.loads(line)
                except json.JSONDecodeError as exc:
                    parse_errors += 1
                    print(f"JSON ERROR {path}:{lineno}: {exc}")
                    continue

                if row.get("serial_equivalent") is False:
                    serial_failures += 1
                    print(f"STATE ERROR {path}:{lineno}: serial_equivalent=false")

                for key, value in walk_numbers(row):
                    if isinstance(value, float) and not math.isfinite(value):
                        nonfinite += 1
                        print(f"NONFINITE {path}:{lineno}: {key}={value}")
                    leaf = key.rsplit(".", 1)[-1]
                    if value < 0 and (
                        leaf.endswith("_nanos")
                        or leaf.endswith("_micros")
                        or leaf.endswith("_millis")
                        or leaf.endswith("_seconds")
                    ):
                        negative_times += 1
                        print(f"NEGATIVE TIME {path}:{lineno}: {key}={value}")

    print(
        f"result integrity: files={len(files)} records={records} "
        f"parse_errors={parse_errors} serial_failures={serial_failures} "
        f"nonfinite={nonfinite} negative_times={negative_times}"
    )
    if parse_errors or serial_failures or nonfinite or negative_times:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
