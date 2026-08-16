#!/usr/bin/env python3
"""Candidate-miss classification policy shared by ConflictLab 1.0 evaluation tools.

Schema-3 records aggregate candidate misses at block level. They do not retain the concrete
profile pair for each miss, so V1.0 can classify the *source capability* from controlled campaign
parameters, but cannot retroactively attribute a miss to an exact relationship without rerunning
with additional telemetry.
"""
from __future__ import annotations

from collections import Counter

INJECTED_PREDICTION_FAULT = "injected-prediction-fault"
RUNTIME_ONLY_DEPENDENCY = "runtime-only-dependency"
STATE_DERIVED_SYMBOLIC_KEY = "state-derived-symbolic-key"
COARSE_SYMBOLIC_GRANULARITY = "coarse-symbolic-granularity"
UNEXPECTED_INPUT_RESOLVED = "unexpected-input-resolved"

MISS_CLASS_ORDER = (
    INJECTED_PREDICTION_FAULT,
    RUNTIME_ONLY_DEPENDENCY,
    STATE_DERIVED_SYMBOLIC_KEY,
    COARSE_SYMBOLIC_GRANULARITY,
    UNEXPECTED_INPUT_RESOLVED,
)

MISS_CLASS_LABELS = {
    INJECTED_PREDICTION_FAULT: "injected prediction fault",
    RUNTIME_ONLY_DEPENDENCY: "runtime-only dependency",
    STATE_DERIVED_SYMBOLIC_KEY: "state-derived symbolic key",
    COARSE_SYMBOLIC_GRANULARITY: "coarse symbolic granularity",
    UNEXPECTED_INPUT_RESOLVED: "unexpected input-resolved miss",
}


def parameter(record, key, default=None):
    return record.get("metadata", {}).get("parameters", {}).get(key, default)


def candidate_misses(record) -> int:
    value = record.get("feedback", {}).get("candidate_misses", 0)
    return int(value) if isinstance(value, (int, float)) else 0


def classify_candidate_miss(record):
    """Return a V1.0 miss class, or ``None`` when the record has no candidate misses."""
    if candidate_misses(record) <= 0:
        return None

    experiment_id = record.get("metadata", {}).get("experiment_id")
    fault_mode = parameter(record, "prediction_fault_mode")
    operation_mix = parameter(record, "operation_mix")
    symbolic_granularity = parameter(record, "symbolic_granularity")

    # Deliberate analyzer false negatives are a controlled fault-injection outcome.
    if (
        experiment_id == "conflictlab-v1-prediction-fault-recovery"
        and fault_mode == "hidden-key"
    ):
        return INJECTED_PREDICTION_FAULT

    # bank-mixed deliberately crosses host/native-bank state that is only visible at runtime.
    if operation_mix == "bank-mixed":
        return RUNTIME_ONLY_DEPENDENCY

    # The symbolic-granularity campaign deliberately removes fine key resolution. Resource/profile
    # modes are controlled coarse abstractions, and point-mixed also contains ConditionalCredit,
    # whose access guard depends on stored EPOCH state. Adaptive planning may therefore omit an
    # unresolved coarse relationship and rediscover a rare concrete conflict at runtime. Fine
    # point-mixed remains strict: its input-resolved keys must not be silently excused here.
    if (
        experiment_id == "conflictlab-v1-symbolic-granularity"
        and operation_mix == "point-mixed"
        and symbolic_granularity in {"resource", "profile"}
    ):
        return COARSE_SYMBOLIC_GRANULARITY

    # These mixes contain operations such as CancelOrder whose concrete BALANCES key comes from
    # contract state rather than transaction input. Unknown static relationships may therefore be
    # omitted by an adaptive materialization threshold and later rediscovered by canonical execution.
    if operation_mix in {"stateful-mixed", "full"}:
        return STATE_DERIVED_SYMBOLIC_KEY

    # Everything else in the V1 suite is expected to be input-resolved and therefore miss-free.
    return UNEXPECTED_INPUT_RESOLVED


def has_recovery_evidence(record) -> bool:
    """Whether a miss left durable/history evidence for conservative future planning."""
    adaptive = record.get("adaptive_state", {})
    feedback = record.get("feedback", {})
    return any(
        int(value or 0) > 0
        for value in (
            adaptive.get("candidate_miss_history_relationships"),
            adaptive.get("runtime_fallback_relationships"),
            feedback.get("fallback_edges_created"),
        )
    )


def record_identity(record):
    metadata = record.get("metadata", {})
    return (
        metadata.get("experiment_id"),
        parameter(record, "operation_mix"),
        metadata.get("mode"),
        metadata.get("seed"),
        metadata.get("run_index"),
        candidate_misses(record),
    )


def validate_candidate_miss_policy(records):
    """Validate V1.0 miss semantics and return aggregate class counters.

    Natural state-derived/runtime-only misses, controlled coarse-granularity misses, and deliberate
    hidden-key misses are accepted only when the record also contains feedback/history evidence.
    Fine input-resolved misses remain fatal.
    """
    miss_totals = Counter()
    miss_records = Counter()
    unexpected = []
    missing_recovery = []

    for record in records:
        misses = candidate_misses(record)
        if misses <= 0:
            continue
        classification = classify_candidate_miss(record)
        miss_totals[classification] += misses
        miss_records[classification] += 1
        if classification == UNEXPECTED_INPUT_RESOLVED:
            unexpected.append(record_identity(record))
        elif not has_recovery_evidence(record):
            missing_recovery.append(record_identity(record))

    if unexpected:
        raise ValueError(f"unexpected input-resolved candidate misses, first={unexpected[:5]}")
    if missing_recovery:
        raise ValueError(
            "candidate misses lacked fallback/miss-history recovery evidence, "
            f"first={missing_recovery[:5]}"
        )

    return miss_totals, miss_records
