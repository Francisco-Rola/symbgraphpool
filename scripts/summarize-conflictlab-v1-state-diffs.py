#!/usr/bin/env python3
"""Summarize opt-in ConflictLab serial/adaptive state mismatch snapshots."""
from __future__ import annotations

import argparse
import csv
import json
from collections import Counter
from pathlib import Path
from typing import Any


def query_kind(request: Any) -> str:
    if isinstance(request, dict) and request:
        return str(next(iter(request)))
    return "unknown"


def response_scalar(value: Any) -> str:
    if isinstance(value, dict):
        for key in ("amount", "value", "count", "epoch", "fee_bps"):
            if key in value and isinstance(value[key], (str, int, float, bool)):
                return str(value[key])
    if value is None:
        return "null"
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("state_diff_dir", type=Path)
    parser.add_argument("--output-dir", type=Path, default=None)
    args = parser.parse_args()

    source = args.state_diff_dir.resolve()
    output = (args.output_dir or source).resolve()
    output.mkdir(parents=True, exist_ok=True)

    rows: list[dict[str, Any]] = []
    run_counts: Counter[str] = Counter()
    kind_counts: Counter[str] = Counter()

    files = sorted(source.glob("*.json"))
    for path in files:
        document = json.loads(path.read_text(encoding="utf-8"))
        run = document.get("run", {})
        exp_id = document.get("experiment_id", "")
        run_key = f"{exp_id}#{run.get('run_index')}:{run.get('mode')}:seed={run.get('seed')}"
        serial = document.get("serial_state") or {}
        adaptive = document.get("adaptive_state") or {}
        serial_queries = {item.get("index"): item for item in serial.get("queries", [])}
        adaptive_queries = {item.get("index"): item for item in adaptive.get("queries", [])}
        mismatch_count = 0
        for index in sorted(set(serial_queries) | set(adaptive_queries), key=lambda x: (-1 if x is None else x)):
            left = serial_queries.get(index)
            right = adaptive_queries.get(index)
            if left == right:
                continue
            request = (left or right or {}).get("request")
            kind = query_kind(request)
            rows.append(
                {
                    "file": path.name,
                    "experiment_id": exp_id,
                    "run_index": run.get("run_index"),
                    "mode": run.get("mode"),
                    "seed": run.get("seed"),
                    "query_index": index,
                    "kind": kind,
                    "contract": (left or right or {}).get("contract"),
                    "request": json.dumps(request, sort_keys=True, separators=(",", ":")),
                    "serial_present": (left or {}).get("contract_present"),
                    "adaptive_present": (right or {}).get("contract_present"),
                    "serial_response": json.dumps((left or {}).get("response"), sort_keys=True, separators=(",", ":")),
                    "adaptive_response": json.dumps((right or {}).get("response"), sort_keys=True, separators=(",", ":")),
                    "serial_scalar": response_scalar((left or {}).get("response")),
                    "adaptive_scalar": response_scalar((right or {}).get("response")),
                }
            )
            mismatch_count += 1
            kind_counts[kind] += 1

        serial_bank = {item.get("address"): item for item in serial.get("bank_balances", [])}
        adaptive_bank = {item.get("address"): item for item in adaptive.get("bank_balances", [])}
        for address in sorted(set(serial_bank) | set(adaptive_bank)):
            left = serial_bank.get(address)
            right = adaptive_bank.get(address)
            if left == right:
                continue
            rows.append(
                {
                    "file": path.name,
                    "experiment_id": exp_id,
                    "run_index": run.get("run_index"),
                    "mode": run.get("mode"),
                    "seed": run.get("seed"),
                    "query_index": "",
                    "kind": "bank_balance",
                    "contract": address,
                    "request": "uconflict",
                    "serial_present": "",
                    "adaptive_present": "",
                    "serial_response": json.dumps(left, sort_keys=True, separators=(",", ":")),
                    "adaptive_response": json.dumps(right, sort_keys=True, separators=(",", ":")),
                    "serial_scalar": "" if left is None else str(left.get("amount")),
                    "adaptive_scalar": "" if right is None else str(right.get("amount")),
                }
            )
            mismatch_count += 1
            kind_counts["bank_balance"] += 1
        run_counts[run_key] = mismatch_count

    csv_path = output / "state-mismatch-details.csv"
    fields = [
        "file",
        "experiment_id",
        "run_index",
        "mode",
        "seed",
        "query_index",
        "kind",
        "contract",
        "request",
        "serial_present",
        "adaptive_present",
        "serial_response",
        "adaptive_response",
        "serial_scalar",
        "adaptive_scalar",
    ]
    with csv_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)

    lines = [
        "# ConflictLab V1 state mismatch summary",
        "",
        f"diagnostic_files={len(files)}",
        f"mismatched_state_entries={len(rows)}",
        "",
        "## Mismatches by query/state kind",
    ]
    if kind_counts:
        for kind, count in kind_counts.most_common():
            lines.append(f"- {kind}: {count}")
    else:
        lines.append("- none")
    lines.extend(["", "## Mismatched entries per reproduced run"])
    for key, count in sorted(run_counts.items()):
        lines.append(f"- {key}: {count}")
    if not run_counts:
        lines.append("- none")
    lines.extend(["", f"details_csv={csv_path}"])
    summary = "\n".join(lines) + "\n"
    (output / "state-mismatch-summary.txt").write_text(summary, encoding="utf-8")
    print(summary, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
