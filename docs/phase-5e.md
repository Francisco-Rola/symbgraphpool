# Phase 5E — learned serialization cost and stable evaluation records

Phase 5E turns the cost-aware policy and diagnostics from Phases 5C/5D into a stable,
workload-independent measurement substrate. All wall-clock measurements remain validator-local
optimization evidence. They do not participate in canonical validation, state transition validity,
or consensus-visible output.

## 5E.1 — learned serialization cost

Phase 5D compared expected replay penalty with a fixed `serialization_cost_reference_nanos`
(default 250,000 ns). Phase 5E retains that value only as a low-confidence fallback and learns a
per-profile-relationship serialization cost from realized READY-DAG execution.

Every speculative transaction records validator-local start/completion offsets and service duration.
For each scheduled dependency `p -> s`, Phase 5E computes:

```text
alternate_ready(p -> s) =
    max(completion(other predecessor of s))
    or phase origin when p is the only predecessor

marginal_serialization_cost(p -> s) =
    max(0, completion(p) - alternate_ready(p -> s))
```

This is the marginal delay that this dependency contributed to the successor becoming READY under
the realized schedule. The observation is attributed back to the static or runtime-discovered
profile relationship and accumulated with the same epoch decay/confidence policy used by the other
adaptive statistics.

A measured zero delay is valid evidence: it means another predecessor already dominated readiness.
Zero confidence, rather than zero cost, represents missing evidence.

The cost-aware candidate policy uses a confidence-weighted effective serialization cost:

```text
effective_serialization_cost =
    lerp(configured_fallback, learned_serialization_cost, serialization_cost_confidence)
```

The Phase 5D replay-risk equation then compares expected replay penalty against this learned value.
When no serialization evidence exists, behavior is identical to Phase 5D because the configured
fallback is used unchanged.

## 5E.2 — feedback persistence

`AdaptiveFeedbackStore` now keeps `SerializationCostStatistics` beside the Beta conflict posterior
and replay-cost/fan-out state for every static and runtime-discovered relationship. It stores:

- decayed weighted marginal serialization nanoseconds;
- decayed observation weight;
- projected confidence;
- raw observation count;
- cumulative serialization nanoseconds.

Feedback checkpoint format is now **version 3**:

- v1 restores conflict statistics and initializes replay/serialization cost state empty;
- v2 restores conflict + replay-cost state and initializes serialization cost state empty;
- v3 restores all three models.

Stable profile keys remain the persistence identity.

## 5E.3 — stable experiment schema

The runtime workspace now contains `acg-evaluation`. `ExperimentRecord` schema version 3 provides a
single machine-readable record for future ConflictLab, MiniWarehouse, and external benchmark
adapters. It includes:

- experiment identity, timestamp, mode, seed, worker/physical-core budget, build/revision metadata,
  deterministic host/environment metadata, and deterministic key/value workload parameters;
- adaptive planning-stage timings;
- candidate-edge classes, raw probability/risk aggregates, pre/post transitive-reduction dependency counts, scheduled hard/soft dependencies, and learned replay/serialization-cost evidence counts;
- serial-equivalent work, serial-cost DAG bound, observed-service critical-path bound, aggregate observed service, worker-capacity bound, corrected feasible parallel lower bound, service inflation, and both legacy/corrected scheduler realization when a serial reference is supplied;
- READY-DAG execution timings/concurrency;
- VM acquisition/entrypoint/recycle, host callback, MVCC, receipt, and cache diagnostics;
- speculative reuse/invalidation/replay and post-consensus reconciliation timings;
- raw conflict/replay/serialization feedback counts, batched mutation counts, and update overhead;
- measured adaptive-pipeline stage timings and total wall from planning through post-consensus feedback,
  plus a serial-reference end-to-end speedup;
- optional canonical/serial state digests and an explicit serial-equivalence result.

Records serialize deterministically to JSON and newline-delimited JSON (`JSONL`). Schema v1 and v2
remain readable; schema v3 is emitted for new runs. Unknown schema versions are rejected rather than
silently interpreted.

## 5E.4 — instrumentation boundary

Per-transaction timestamps are attached to speculative receipts and validator execution reports.
They are observational only. A validator may measure different nanoseconds, learn different costs,
and therefore choose a different speculative schedule. Canonical correctness is still enforced by
concrete receipt dependencies, canonical-order validation, and replay.

## Focused validation

These checks are part of the canonical repository gate:

```bash
./scripts/run-all-tests.sh
```

The regression coverage checks:

1. serialization-cost decay/checkpoint behavior and v1/v2 -> v3 compatibility;
2. candidate risk changes when replay evidence is identical but serialization cost differs;
3. deterministic experiment-record JSON/JSONL schema round trips and schema-version rejection;
4. a two-worker runtime integration where a deliberately slow hard dependency produces a measured
   marginal READY delay, persists it, consumes it in the next block's candidate edge, and emits a
   the current experiment record schema.

The runner writes logs, a human-readable summary, and `records.jsonl` under
`benchmark-results/phase5e-measurement/<timestamp>/`.

## Remaining work

Phase 5E establishes the measurement and learned-cost substrate but does not yet define publication
acceptance thresholds. Phase 5F should freeze correctness, reproducibility, adaptation, and
performance gates and then the common benchmark adapter/experiment matrix can be expanded without
changing metric definitions.
