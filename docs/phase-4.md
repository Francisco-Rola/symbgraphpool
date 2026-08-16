# Phase 4 — Adaptive weighted scheduling

Phase 4 consumes the runtime feedback learned in Phase 3 and turns it into scheduling decisions.
It remains outside the consensus correctness boundary: a schedule is an optimization hint, and
parallel speculative execution/validation is still deferred to Phase 5.

## Phase 4A — scheduler-facing adaptive state

`acg-feedback` exposes projected, non-mutating edge estimates for static and runtime-discovered
relationships. Candidate-miss history is persisted separately from ordinary positive evidence so a
concrete runtime miss can disable absolute symbolic pruning without making every historically
positive profile relationship an override.

Runtime-discovered fallback edges are indexed by dense `ProfileId` for online adjacency traversal.

## Phase 4B — weighted candidate graph

`CandidateGraphBuilder::build_weighted` combines symbolic predicates with adaptive posterior state.
Concrete transaction edges carry Q16 probability/confidence, conflict kinds, predicate result, and
provenance (`Static` or `RuntimeDiscovered`).

A symbolic `False` is still pruned unless that static relationship has concrete candidate-miss
history. Runtime-discovered fallback topology participates in future candidate construction.

## Phase 4C — hard/soft classification and risk-bounded waves

`RiskBoundedScheduler` classifies each materialized transaction edge by probability:

```text
p >= hard_threshold                 -> hard dependency
soft_threshold <= p < hard_threshold -> soft conflict
p < soft_threshold                  -> low / ignored by primary scheduling
```

Hard dependencies are oriented by a deterministic predicted-order-compatible priority. Predicted
position is primary; inclusion probability, estimated execution cost, conflict degree, and finally
`TxIndex` break ties.

Soft edges contribute same-wave placement risk:

```text
risk(tx, wave) = 1 - product(1 - p(tx, u))
```

The scheduler greedily places each transaction in the earliest wave that:

1. occurs after all hard predecessors;
2. keeps the transaction's accumulated soft risk within `risk_budget`; and
3. satisfies optional `max_wave_width` capacity.

The scheduler deliberately uses the probability thresholds from the design directly. Q16
confidence remains available on candidate edges for metrics and later policy experiments, but Phase
4C does not invent a confidence-adjusted probability rule that the design has not specified.

`RiskBoundedSchedule::validate_against` replays the deterministic placement order and verifies hard
dependencies, placement risk, wave capacity, uniqueness, and completeness.

## Correctness boundary

Phase 4C can produce waves wider than one, but the existing serial runtime executor must not treat
that as permission for unsafe concurrent commit. Runtime integration may inspect or flatten the
schedule until Phase 5 adds isolated speculative execution, canonical validation, invalidation, and
selective replay.

## Phase 4D — validator integration with serial correctness boundary

`acg-runtime-feedback::AdaptiveSerialPipeline` now connects the runtime-facing pieces end to end:

```text
ProducedBlock
    -> CosmWasmCandidateAdapter
    -> weighted CandidateGraph using current feedback state
    -> RiskBoundedScheduler
    -> speculative ExecutionPlan (possibly wide waves)
    -> canonical serial ExecutionPlan
    -> CosmWasm execution/access traces
    -> RuntimeFeedbackEngine
    -> updated edge statistics for the next block
```

Planning uses `block.context.height` as the current feedback epoch for the initial integrated
pipeline. This is an explicit baseline choice; the design still leaves the long-term decay unit as
an experimental decision.

The adaptive wave plan is preserved for inspection and metrics, but Phase 4D never submits a wide
wave to `SerialBlockExecutor`. Actual execution remains one transaction at a time in block order.
This keeps scheduling advisory until Phase 5 provides isolated speculative state, validation, and
selective replay.

The integration tests cover both intended benchmark roles:

- ConflictLab verifies exact symbolic independence/conflict grouping, canonical serial execution,
  and positive concrete conflict learning in one end-to-end block.
- MiniWarehouse uses deterministic repeated symbolic Restock contention with a no-access runtime
  fixture to verify that canonical negative evidence changes a future edge from hard to soft and
  eventually permits same-wave placement.
