#!/usr/bin/env python3
"""Consensus-window-aware performance metrics for ConflictLab experiment records."""

from __future__ import annotations

import math
from typing import Any


def consensus_pipeline_metrics(
    record: dict[str, Any],
    preconsensus_window_ms: float | None = None,
) -> dict[str, float | bool | None]:
    """Return consensus-pipelined timing metrics for one experiment record.

    `preconsensus_window_ms` is the amount of wall-clock time available for work
    that is eligible to execute before consensus decides the block. When it is
    omitted, reporting assumes all eligible work completes before consensus.

    This applies equally to speculative execution and direct-serial fallback:
    serial fallback execution is pre-execution and can therefore produce reusable
    canonical results before consensus. If the window expires first, only the
    unfinished eligible work spills onto the post-consensus critical path.

    Phase 6+ records carry an actual measured cutoff split and proposal/decision
    divergence diagnostics; those values are used directly when no override window
    is requested. Older Phase-5 records retain the analytical fallback model.
    """

    if preconsensus_window_ms is not None and preconsensus_window_ms < 0.0:
        raise ValueError("preconsensus_window_ms must be non-negative")

    # Phase 6+ records contain a physically measured cutoff split. Prefer it whenever the caller
    # did not request a legacy/counterfactual reporting window. Older records deserialize with a
    # zero cutoff and continue through the analytical Phase-5 path below.
    recorded = record.get("consensus") or {}
    if preconsensus_window_ms is None and float(recorded.get("cutoff_nanos", 0) or 0) > 0.0:
        window_nanos = float(recorded["cutoff_nanos"])
        pre_completed = float(recorded.get("pre_consensus_nanos", 0) or 0)
        spill = float(recorded.get("pre_consensus_overrun_nanos", 0) or 0)
        post = float(recorded.get("post_consensus_nanos", 0) or 0)
        bottleneck = float(recorded.get("bottleneck_nanos", 0) or 0)
        serial = float(
            recorded.get("serial_validation_latency_nanos")
            or record["pipeline_timing"]["serial_reference_execution_nanos"]
        )
        transactions = float(
            recorded.get("decided_transactions") or record["execution"]["transactions"]
        )
        eligible_pre = pre_completed + spill
        intrinsic_post = max(post - spill, 0.0)
        validation_speedup = serial / post if post > 0.0 else math.inf
        throughput_speedup = serial / bottleneck if bottleneck > 0.0 else math.inf
        return {
            "preconsensus_window_nanos": window_nanos,
            "preconsensus_eligible_nanos": eligible_pre,
            "preconsensus_completed_nanos": pre_completed,
            "preconsensus_spill_nanos": spill,
            "intrinsic_postconsensus_nanos": intrinsic_post,
            "postconsensus_validation_nanos": post,
            "pipeline_bottleneck_nanos": bottleneck,
            "preexecution_complete_before_consensus": spill == 0.0,
            "serial_validation_latency_nanos": serial,
            "validation_latency_speedup": validation_speedup,
            "throughput_speedup": throughput_speedup,
            "serial_throughput_blocks_per_s": 1e9 / serial if serial > 0.0 else math.inf,
            "acg_throughput_blocks_per_s": 1e9 / bottleneck if bottleneck > 0.0 else math.inf,
            "serial_throughput_tps": transactions * 1e9 / serial if serial > 0.0 else math.inf,
            "acg_throughput_tps": transactions * 1e9 / bottleneck if bottleneck > 0.0 else math.inf,
        }

    timing = record["pipeline_timing"]
    planning = float(timing.get("planning_nanos", 0))
    preexecution = float(timing.get("preexecution_nanos", 0))
    pre_feedback = float(timing.get("pre_execution_feedback_nanos", 0))
    reconciliation = float(timing.get("reconciliation_nanos", 0))
    reconciliation_feedback = float(timing.get("reconciliation_feedback_nanos", 0))
    serial = float(timing["serial_reference_execution_nanos"])
    transactions = float(record["execution"]["transactions"])

    eligible_pre = planning + preexecution + pre_feedback
    intrinsic_post = reconciliation + reconciliation_feedback

    if preconsensus_window_ms is None:
        window_nanos: float | None = None
        pre_completed = eligible_pre
        spill = 0.0
    else:
        window_nanos = preconsensus_window_ms * 1e6
        pre_completed = min(eligible_pre, window_nanos)
        spill = max(eligible_pre - window_nanos, 0.0)

    post = spill + intrinsic_post
    bottleneck = max(pre_completed, post)
    validation_speedup = serial / post if post > 0.0 else math.inf
    throughput_speedup = serial / bottleneck if bottleneck > 0.0 else math.inf

    return {
        "preconsensus_window_nanos": window_nanos,
        "preconsensus_eligible_nanos": eligible_pre,
        "preconsensus_completed_nanos": pre_completed,
        "preconsensus_spill_nanos": spill,
        "intrinsic_postconsensus_nanos": intrinsic_post,
        "postconsensus_validation_nanos": post,
        "pipeline_bottleneck_nanos": bottleneck,
        "preexecution_complete_before_consensus": spill == 0.0,
        "serial_validation_latency_nanos": serial,
        "validation_latency_speedup": validation_speedup,
        "throughput_speedup": throughput_speedup,
        "serial_throughput_blocks_per_s": 1e9 / serial if serial > 0.0 else math.inf,
        "acg_throughput_blocks_per_s": 1e9 / bottleneck if bottleneck > 0.0 else math.inf,
        "serial_throughput_tps": transactions * 1e9 / serial if serial > 0.0 else math.inf,
        "acg_throughput_tps": transactions * 1e9 / bottleneck if bottleneck > 0.0 else math.inf,
    }
