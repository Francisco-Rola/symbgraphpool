# Implementation status

This document is the repository checkpoint after Brick 5E, with Brick 5C.7 as the production execution substrate and VM-lifecycle work preserved as research.

## Production implementation

### Brick 1 — symbolic profile graph foundation — implemented

- parse and validate analyzer JSON;
- normalize symbolic profiles;
- stable content-derived profile identity;
- dense validator-local `ProfileId` assignment;
- indexed offline profile-edge derivation;
- portable graph artifact + immutable CSR graph;
- compiler/inspection CLI;
- ConflictLab, MiniWarehouse, and Astroport fixtures/profiles;
- minimal deterministic CosmWasm runtime and validator simulation substrate.

### Brick 2 — concrete transaction graph — implemented

- runtime transactions are adapted to `ProfileId`, `InstanceId`, and concrete input bindings;
- precompiled three-valued predicates evaluate concrete candidate relationships;
- only persistent profile adjacency is traversed;
- `False` prunes, while `True` and `Unknown` materialize conservative candidate edges.

### Brick 2.1 — clause-level conflict resolution — implemented

- alternative conflict clauses are classified independently;
- explicit unknown-reason metadata is retained;
- contract-local relationships use dense instance identity, preventing deployments of the same code from colliding solely by shared code hash.

### Brick 2.5 — structured MiniWarehouse integration — implemented

- deterministic workload generator;
- sparse/bootstrap state setup;
- variable order-line bindings;
- input-derived `(warehouse, district, order)` prefixes;
- remote-stock and skew controls;
- structured ConflictLab/MiniWarehouse acceptance coverage.

### Brick 3 — runtime feedback and adaptive statistics — implemented

- concrete execution/access attribution;
- positive and negative evidence;
- decayed Beta statistics per static relationship;
- runtime-discovered fallback edges for symbolic topology misses;
- distinct evidence weights for pre-execution/canonical/validation/replay;
- stable-key checkpoints across dense-ID reassignment.

### Brick 4 — adaptive weighted scheduling — implemented

- feedback-projected weighted candidate graph;
- runtime fallback topology participates in candidate construction;
- configurable hard/soft probability thresholds;
- deterministic hard-dependency orientation;
- cumulative-risk-bounded wave construction;
- schedule validation;
- integrated adaptive planning pipeline.

Scheduler waves are diagnostic/planning structures, not execution barriers.

### Brick 5A — isolated speculative state and receipts — implemented

- detached transaction-visible state;
- speculative execution without canonical mutation;
- commit-ready write sets;
- correctness read dependencies for point/range storage, bank state, all-balances, and contract metadata;
- reverted-child read preservation and reverted-write exclusion.

### Brick 5B — canonical validation, reuse, and selective replay — implemented

- receipts are bound to request/block/engine identity;
- validation occurs in canonical transaction order;
- valid successful receipts atomically commit detached writes and reuse outputs;
- valid failed receipts are reused without writes;
- invalid receipts replay canonically;
- range/all-balances phantom detection is part of validation.

### Brick 5C historical strict-wave prototype — removed from production

The Rayon strict-wave runtime proved concurrent detached receipt execution but introduced global barriers and an inadequate predecessor visibility model. It has been superseded.

### Brick 5C.5 — split-phase timeline — implemented

- N+1 graph/schedule planning may overlap N validation;
- N+1 transaction execution starts only after N has canonically committed;
- pre-execution snapshots therefore use committed predecessor state.

### Brick 5C.6 — dependency-driven versioned pre-execution — superseded by 5C.7

This stage introduced successor-driven launch and versioned predecessor visibility. Its useful semantics remain in 5C.7; historical reconstruction machinery was removed.

### Brick 5C.7 — READY-DAG + block-local persistent MVCC — implemented

- no global wave barriers;
- workers launch a transaction as soon as all predictive predecessors are complete;
- one immutable block base is shared by the predicted block;
- successful speculative transactions publish block-local storage/bank/contract versions;
- each launch captures a compact immutable visibility mask;
- reads resolve the newest visible canonical-earlier version lazily, then fall through to the block base;
- transaction-local writes remain private until successful receipt completion;
- failed transactions publish no versions;
- concrete receipt validation/replay remains the correctness authority for missed/soft/unknown conflicts.

## Performance findings that affect design

These findings are research evidence, not additional production features:

- the READY-DAG scheduler is generally within a few percent of the observed-service DAG lower bound;
- controlled six/four/two/one-lane compute workloads follow their dependency ceilings closely;
- compute-heavy fully independent Wasm reaches about 5.2x wall-clock speedup on six physical cores;
- small transactions suffer large fixed/concurrent service-cost inflation;
- ordinary conflict-free point MVCC is not the dominant source of that inflation;
- VM lifecycle is material: unsafe retained instances provide a large upper-bound improvement but violate isolation;
- fresh cache sharding preserves tested fresh semantics but gives only small/inconsistent end-to-end gains;
- deeper VM snapshot/reset/COW work remains research, not production.

See `research/vm-lifecycle/README.md` for the archived VM work.

### Brick 5D — cost-aware validation/replay policy — implemented

- 5D.1: concrete reconciliation attribution retains the exact stale validation dependency, responsible canonical predecessor, measured direct replay cost, candidate-edge presence, and transitive replay fan-out;
- 5D.2: decayed replay-cost/fan-out statistics are persisted beside conflict probability; Brick 5E checkpoint v3 remains backward-compatible with v1/v2;
- 5D.3: candidate edges retain raw conflict probability separately from cost-adjusted scheduling risk, and the risk-bounded scheduler uses the latter;
- 5D.4: closed-loop phase-change tests prove expensive replay evidence hardens future scheduling and later independence/decay relaxes it again.

Measured wall time remains validator-local optimization evidence and never becomes a correctness or consensus input. See [`brick-5d.md`](brick-5d.md).

## Remaining proposed Brick 5 work

### Brick 5E — learned serialization cost + stable evaluation records — implemented

- READY-DAG transactions expose validator-local start/completion/service timing for performance attribution;
- each scheduled edge learns decayed marginal dependency-ready delay as its serialization cost;
- 5D's configured 250 us serialization reference is now only a low-confidence fallback and is blended toward learned per-relationship cost;
- feedback checkpoints are v3 with v1/v2 backward-compatible restore;
- the `acg-evaluation` runtime crate defines schema-v1 deterministic JSON/JSONL experiment records spanning planning, scheduling, DAG bounds, VM/host/MVCC execution, replay, feedback overhead, and correctness digests.

See [`brick-5e.md`](brick-5e.md).

### Brick 5F — serial-equivalence and performance acceptance — partially implemented / remaining gate

Correctness coverage exists across speculative receipts, validation/replay, ConflictLab, MiniWarehouse, and MVCC. A final production acceptance matrix and stable performance thresholds still need to be defined.

## Proposed follow-on engineering

1. Define Brick 5F acceptance criteria on supported hardware and workloads, including closed-loop adaptation and checkpoint compatibility.
2. Keep VM snapshot/reset/COW work on a separate research branch until it passes adversarial isolation tests (memory, globals, tables, gas, memory growth, traps/OOG, backends) and end-to-end serial equivalence.
3. Add scan/iterator-specific storage probes if MiniWarehouse-specific storage inflation remains material.
4. Consider cost/granularity-aware worker/concurrency policy and explicit soft-edge exploration only as correctness-independent optimization hints.
