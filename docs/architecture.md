# Adaptive Conflict Graph architecture

ACG separates **prediction**, **speculative execution**, and **canonical correctness**.

```text
symbolic contract profiles
        ↓
prepared profile graph + compiled predicates
        ↓
block transactions → concrete candidate relationships
        ↓
runtime probability/confidence + cost/risk policy
        ↓
compact, transitively reduced dependency-ready DAG
        ↓
pre-consensus Wasmd/WasmVM execution over block-local MVCC
        ↓
detached receipts + exact read/range/write observations
        ↓
consensus decides canonical block/order
        ↓
key-indexed validation → receipt reuse or selective replay
        ↓
serial-equivalent committed state
```

## Offline model

Analyzer output is normalized into stable profile identities and input-dependent conflict clauses. Predicates are compiled when the profile graph is loaded. `StableProfileKey` is persistent identity; dense `ProfileId`, contract `InstanceId`, and block-local transaction indexes are runtime-only handles.

## Candidate graph and scheduling

Only profile/instance combinations that can interact are compared. Proven conflict cliques can be represented as equivalence groups instead of quadratic explicit edges. Runtime evidence tracks conflict probability, confidence/maturity, replay/fan-out cost and candidate misses. The risk-bounded scheduler classifies relationships as Low/Soft/Hard, applies cost/risk policy, and exact-transitively-reduces the final atomic transaction DAG, including adapter-level bank/funds dependencies.

Unknown or unsupported symbolic information is conservative; prediction is never a correctness assumption.

## Execution and reconciliation

Rust owns symbolic graph construction, adaptive state and scheduling. Go/Wasmd owns the Cosmos SDK/WasmVM execution substrate. Dependency-ready transactions execute in isolated branches with block-local MVCC visibility and produce detached receipts/deltas. Concrete read, range-read and write observations are recorded.

After the consensus boundary, reconciliation walks canonical order. Indexed validation checks only keys/ranges that could have become stale. Valid receipts are reused; stale transactions are selectively replayed. The complete application state is checked against an independently initialized Serial oracle in evaluation.

## Adaptive control plane

The runtime supports decayed conflict probability/confidence, runtime-discovered fallback relationships, bounded exploration, serialization/replay-cost learning, direct serial bypass when speculation is economically unattractive, regime-change detection/probation, and candidate-vs-decided/consensus-cutoff handling.

## Evaluation boundary

The common Wasmd evaluator contains Serial, Cosmos SDK BlockSTM, faithful Wasmd ports of AriaFB and Vegeta, and Rust-ACG. Single-node paper throughput measures consensus-visible replay/post work; it does not pretend local pre-consensus timing is consensus latency. See `evaluation/PAPER_PLAN.md`.
