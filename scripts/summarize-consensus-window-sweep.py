#!/usr/bin/env python3
"""Report Phase 5 validation/throughput sensitivity to the consensus execution window."""

from __future__ import annotations

import argparse
import json
import statistics
from pathlib import Path
from typing import Any

from consensus_pipeline_metrics import consensus_pipeline_metrics


def load(path: Path) -> list[dict[str, Any]]:
    with path.open("r", encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def median(values) -> float:
    return statistics.median(list(values))


def fmt_speed(value: float) -> str:
    return "inf" if value == float("inf") else f"{value:.2f}"


def is_b512_bucketed_adaptive(record: dict[str, Any]) -> bool:
    params = record["metadata"]["parameters"]
    return (
        record["metadata"]["experiment_id"] == "conflictlab-phase5-control-plane"
        and params["sim.block_size"] == "512"
        and params["prediction_quality"] == "bucketed"
        and params["acg.serial_bypass_enabled"] == "false"
        and record["metadata"]["mode"] in {"probability-only", "cost-aware"}
    )


def summarize(records: list[dict[str, Any]], windows_ms: list[float]) -> str:
    groups = [
        ("all", records),
        ("speculative", [r for r in records if not r["planning"]["serial_bypassed"]]),
        ("serial-fallback", [r for r in records if r["planning"]["serial_bypassed"]]),
        ("b512-bucketed-adaptive", [r for r in records if is_b512_bucketed_adaptive(r)]),
    ]
    lines = [
        "Phase 5 consensus-window sensitivity",
        "assumption: candidate block == decided block, so pre-executed results remain valid",
        "validation=unfinished eligible pre-execution + intrinsic post-consensus work",
        "throughput service time=max(completed pre-consensus work, post-consensus work)",
        "",
        "window_ms group                    n complete_pct eligible_pre_ms post_ms validation_x throughput_x",
    ]
    for window in windows_ms:
        for label, rows in groups:
            if not rows:
                continue
            metrics = [consensus_pipeline_metrics(r, window) for r in rows]
            complete = sum(bool(m["preexecution_complete_before_consensus"]) for m in metrics)
            lines.append(
                f"{window:8g} {label:24s} {len(rows):3d} "
                f"{100.0 * complete / len(rows):11.1f} "
                f"{median(float(m['preconsensus_eligible_nanos']) for m in metrics)/1e6:15.3f} "
                f"{median(float(m['postconsensus_validation_nanos']) for m in metrics)/1e6:7.3f} "
                f"{fmt_speed(median(float(m['validation_latency_speedup']) for m in metrics)):>12s} "
                f"{median(float(m['throughput_speedup']) for m in metrics):12.2f}"
            )
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("records", type=Path)
    parser.add_argument("--windows-ms", type=float, nargs="+", default=[2, 5, 10, 25, 50])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    text = summarize(load(args.records), args.windows_ms)
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
