# Implementation status

ACG's main execution path is implemented end to end.

## Phase 1 — symbolic profiles

Symbolic contract accesses are normalized into stable profile identities and conflict predicates.
Unknown analyzer information is preserved conservatively.

## Phase 2 — concrete candidate graph

Runtime transaction inputs refine symbolic relationships into concrete candidate conflicts. Clause-
level matching, stable contract identity, and compact equivalence groups are implemented.

## Phase 3 — runtime feedback

Concrete execution produces positive/negative conflict evidence, decayed probability/confidence,
candidate-miss history, and runtime fallback relationships. Checkpoints use stable relationship keys.

## Phase 4 — adaptive scheduling

Candidate relationships carry raw conflict probability and scheduling risk. The scheduler classifies
Low/Soft/Hard relationships, builds a risk-bounded ordering DAG, and performs exact transitive
reduction before execution.

## Phase 5 — safe speculative execution

Implemented runtime behavior:

- detached speculative receipts and transaction-local writes;
- canonical validation, reuse and selective replay;
- dependency-driven READY-DAG execution without global wave barriers;
- block-local persistent MVCC visibility for storage/bank/contract state;
- replay/fan-out and serialization-cost learning;
- serial bypass admission based on previous-block economics;
- schema-v3 experiment records with planning/execution/feedback/consensus timing;
- manifest-driven Phase-5F correctness/provenance acceptance.

Canonical validation/replay remains the correctness authority when prediction is incomplete.

## Current evaluation state

ConflictLab 1.0 provides the main internal mechanism evidence. It covers correctness, prediction
faults, symbolic granularity, compaction, consensus timing/divergence, non-stationary adaptation,
runtime semantics, fixed-six-worker block scaling, policy tuning, VM lifecycle controls and
statistical repetitions.

A dedicated controlled parallelism-ceiling experiment now creates exact 1/2/3/4/6-lane conflict
patterns plus a fully independent block. It compares nominal 6-worker capacity, the hindsight
concrete-conflict oracle, actual executor wall time, phase throughput and full adaptive wall time,
then reports planning/executor/reconciliation/feedback and Wasm/host/MVCC overhead.

## Next engineering work

1. Run the controlled parallelism ceiling on native Linux and use its overhead breakdown to remove
   execution/control-path losses before adding more policy complexity.
2. Add core-count and memory/RSS scaling.
3. Move MiniWarehouse onto the same current harness/reporting methodology.
4. Add external workloads and competitive baselines.
5. Freeze one clean revision for the final publication artifact.

Detailed historical phase notes remain under `docs/phase-*.md`.
