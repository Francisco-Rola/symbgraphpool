# Phase 5 — Safe speculative execution

Phase 5 turns Phase 4's advisory waves into real speculative execution while preserving canonical
serial semantics through validation and replay.

## Phase 5A — isolated speculative state and receipts

Phase 5A introduces the execution substrate only; it does **not** validate, commit speculative
results, replay transactions, or add worker threads yet.

`CosmWasmEngine::snapshot()` captures a detached transaction-visible copy of world state.
`execute_speculative()` runs an instantiate/execute request against that snapshot and returns a
`SpeculativeTxResult` without mutating canonical state.

A receipt contains:

- the success/failure status and successful output metadata;
- attempted access records;
- correctness-level read dependencies;
- a deterministic commit-ready storage/bank/contract write set for successful transactions.

Read dependencies cover contract metadata, point storage reads, storage ranges, point bank reads,
and all-balances enumeration. Reads satisfied by transaction-local writes are not base-state
dependencies. Range/all-balances dependencies retain enough base information to detect phantoms in
Phase 5B.

Reads performed inside reverted child calls are retained because they can influence a handled
failure/reply and therefore remain relevant to correctness validation. Reverted child writes do
not enter the final write set. A failed top-level transaction has an empty commit-ready write set
and its attempted accesses are marked reverted.

The same snapshot can be shared by future worker threads because transactions use independent
transaction overlays and never commit into the snapshot.

## Remaining Phase 5 stages

- **5D:** broader validation/replay-driven adaptive policy beyond the narrow hard-to-soft evidence
  loop introduced by Phase 5C.6.
- **5E:** cost-aware theoretical versus realized parallelism and execution-efficiency metrics.
- **5F:** ConflictLab/MiniWarehouse serial-equivalence and performance acceptance.

## Phase 5B: canonical validation, reuse, and selective replay

Phase 5B keeps execution single-threaded while proving the correctness state machine that Phase 5C will run beneath a worker pool.

A speculative receipt is bound to the exact `ExecutionRequest`, engine, and semantic block context
(height, time, and chain id) that produced it. Canonical transaction position is deliberately *not*
part of receipt identity: a transaction moved by block reordering may still reuse its receipt when
its concrete read set remains valid. Contracts whose semantics depend on `Env.transaction.index` are
out of scope for receipt reuse; such environment dependencies would require analyzer/VM-level
tracking before they could be admitted safely. The canonical coordinator drains transactions
strictly in block order and validates all 5A read dependencies against the current canonical
predecessor state:

- contract metadata/existence;
- point storage reads, including observed absence;
- storage ranges with transaction-local masked keys removed, detecting value changes and insertion/deletion phantoms;
- point bank balances;
- all-balances enumeration with transaction-local masked denominations removed.

A valid successful receipt commits its detached `StateWriteSet` atomically under the canonical
world-state write lock and reuses the speculative events/data/result without re-executing the
contract. A valid failed receipt reuses the failure and commits no writes. A receipt whose concrete
read dependencies became stale after reordering is replayed through the canonical execution path; a
malformed receipt or one from an incompatible semantic block context is rejected/discarded.

Blind writes deliberately do not create read dependencies. Therefore a later canonical blind write can reuse its speculative receipt even when an earlier predecessor wrote the same key: canonical ordering still makes the later write authoritative.

Phase 5B also introduces reuse/validation/replay metrics. Actual worker parallelism and wall-clock speedup remain Phase 5C/5E concerns.


## Phase 5C: historical strict-wave prototype

Phase 5C originally introduced a bounded Rayon strict-wave executor to prove that detached receipts
could execute concurrently and still reconcile through the Phase-5B canonical validator. That
prototype served its purpose but exposed two bad production properties: global wave barriers and a
contiguous-prefix state model that could leave already-completed predecessors invisible.

The strict-wave execution API and Rayon-based execution code were removed in Phase 5C.7. Scheduler waves
remain only as diagnostic dependency levels; they are not runtime barriers. Canonical correctness is
still provided by the Phase-5B concrete dependency validator/replay path.

## Phase 5C.5: split-phase consensus timeline with post-commit speculation

Phase 5C.5 separates three different activities that were previously bundled together:

1. **next-block prediction and graph/schedule planning**;
2. **parallel speculative execution**;
3. **post-consensus validation/replay/commit**.

The important state rule is now stricter: block N+1 may be predicted and its graph/schedule may be
constructed while block N is being validated, but **N+1 transaction execution does not start until
block N has canonically committed**. `SpeculativeParallelBlockExecutor::prepare` snapshots the
canonical engine at that point, so N+1 receipts are always based on the actual post-N world state,
not a guessed successor state. This intentionally trades some speculative lead time for much better
receipt freshness and a cleaner correctness/performance model.

With more than one configured worker, the MiniWarehouse harness overlaps only N+1
graph/schedule planning with the single-threaded post-consensus validation of N. Once N commits,
the full configured worker budget becomes available for N+1 pre-execution because validation and
pre-execution no longer overlap. With one worker, planning is simply performed after N commits.

The consensus window is independently configurable with `ACG_MW_CONSENSUS_MS` and is no longer
constrained to be less than or equal to the mempool block-batching interval. In this benchmark it
represents the design window available, after the predecessor commit, to finish any planning
overhang plus parallel pre-execution before the next block decision. This is a deliberate research
assumption: the benchmark measures whether the implementation fits the configured window rather
than deriving that window from consensus internals. By default
`ACG_MW_REQUIRE_PREEXEC_WITHIN_CONSENSUS=1`; a miss fails the measurement and asks the caller to
increase the window. Setting it to `0` keeps the safe canonical-fallback path for stress tests.

The mempool model remains FIFO and batched. Transaction arrival rate, initial backlog, block size,
block interval, consensus window, worker count, and scheduler thresholds are all configurable. The
benchmark reports admission-to-proposal and admission-to-decision latency, but those are virtual
protocol timestamps; wall-clock `Instant` measurements are reserved for actual planning,
pre-execution, validation, replay, and commit work.

Planning overlap is accounted for explicitly. If planning N+1 takes 5 ms and validation N takes
3 ms, only 3 ms are hidden; the remaining 2 ms of planning overhang plus N+1 pre-execution must fit
inside the configured consensus window. The first block has no predecessor validation phase, so its
full planning plus pre-execution cost is charged to the initial window.

The benchmark continues to report adapter, candidate-graph, scheduler, schedule-validation and
plan-conversion timings separately; within-block pair counts, materialized edge density, predicate
True/Unknown/learned-False counts, top profile-pair expansions, wave widths and cost-aware
parallelism; prediction precision/coverage; deadline hit rate; and post-consensus receipt matching,
validation, replay/missing execution, reused-commit time, and speedup over a serial post-consensus
baseline.


## Phase 5C.6: dependency-driven versioned pre-execution

Phase 5C.6 replaces the split-phase strict-wave launch model with a dependency-driven ready DAG.
The scheduler still emits levels for diagnostics and theoretical analysis, but those levels are no
longer global execution barriers. A transaction becomes runnable as soon as every explicit
predecessor has completed. Independent chains can therefore overlap continuously across scheduler
levels.

Known topology starts conservatively. A concrete symbolic predicate result of `True`, a persisted
historical candidate-miss override, or a runtime-discovered relationship is initially classified
`Hard` regardless of its prior probability. Hard edges are oriented according to predicted/canonical
block position and emitted as execution dependencies, so the successor cannot start before the
predecessor has produced its speculative version. After
`independent_observations_before_softening` concrete *independence* observations, the relationship
may demote to `Soft` when its adaptive posterior falls below the hard threshold. A known relationship is not
demoted directly to `Low`; softening means it becomes eligible for explicit risk-bounded
speculation. Ordinary unresolved `Unknown` relationships continue to use Low/Soft/Hard probability
thresholds.

Known topology is also retained through candidate materialization. The generic materialization
threshold applies to unresolved `Unknown` static relationships; a concrete symbolic `True`, a
historical false-predicate override, and persisted runtime-discovered topology remain materialized
even when their posterior falls below that threshold. This lets execution evidence soften a known
dependency without silently deleting it from the graph.

The 5C.6 logical model publishes each completed successful receipt as a temporary state version
tagged by canonical transaction index. Its first physical implementation materialized each launch
by deep-copying the committed base and replaying all completed earlier-canonical write sets. Phase
5C.7 keeps the semantics but replaces that expensive materialization with block-local MVCC.
Consequently:

- a future-canonical transaction can finish physically before `i` without leaking its writes
  backward into `i`;
- a completed hard predecessor becomes visible immediately even if unrelated earlier-canonical
  transactions are still running;
- unrelated earlier-canonical work is allowed to race, which is deliberate speculation rather than
  a global-prefix barrier; and
- post-consensus canonical reconciliation remains deterministic because detached receipts are
  validated and committed in canonical order.

Soft edges have two outcomes during scheduling. If the risk budget accepts a same-level placement,
the pair executes speculatively with no dependency. If the scheduler separates the pair, it emits a
Soft execution dependency so the later transaction consumes the predecessor's completed version.
Hard edges always emit a dependency and must occupy increasing diagnostic levels. The predictive
graph therefore controls readiness, but it never authorizes canonical reuse: post-consensus concrete
read-dependency validation remains the correctness authority and stale/missing receipts still replay
canonically.

Phase 5C.6 also feeds concrete execution evidence into the adaptive store before the broader Phase
5D policy work. Successful pre-consensus receipts contribute actual read/write overlap and
independence evidence. Transactions that replay post-consensus contribute their corrected concrete access traces against
the final outcomes of the decided block, and the validation dependency that forced each replay
contributes targeted positive evidence. Replay-vs-reused pairs are therefore observed after the
replay, while reused-vs-reused pairs are not counted a second time because both traces were already
observed during pre-execution. This is the evidence loop that allows initially Hard symbolic/runtime
relationships to soften only after repeated concrete independence.

The MiniWarehouse benchmark exposes `ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER` (default `8`; `0` disables
softening) and now reports dependency counts, pre-execution/replay-feedback time, dependency-DAG theoretical lower
bounds, and invalidation attribution in terms of guarded versus unguarded predecessors.

## Phase 5C.7: block-local persistent MVCC speculative state

Phase 5C.7 removes the O(block-size²) state reconstruction cost measured in the first 5C.6
implementation. One detached committed predecessor snapshot is shared by the whole predicted block.
Successful transactions publish only their final storage/bank/contract deltas into a block-local
multi-version index keyed by canonical transaction index.

When transaction `i` becomes ready it captures a compact immutable visibility bit-mask of successful
earlier-canonical transactions that had completed at launch. Point, range, bank, all-balances and
contract-metadata reads lazily resolve the newest version whose writer index is `< i` and is present
in that launch mask, then fall through to the immutable block base. Versions completed after launch
remain invisible for the lifetime of that transaction, so speculative reads are repeatable. A
future-canonical version can never flow backward.

This removes all per-transaction full-world copies and all historical write-set replay from the
READY-DAG path. Transaction-local writes remain private overlays until the receipt completes. A
successful receipt publishes its final delta before its dependency successors are unblocked. Failed
transactions publish no state. The predictive dependency graph still controls readiness only;
concrete receipt validation/replay remains the correctness authority for missed Soft/Unknown
conflicts.

The deprecated strict-wave executor, predicted-successor chaining API, strict-wave theoretical
metrics and Rayon execution code were removed. The exact Rayon workspace pins are retained only as Rust-1.75 dependency-resolution guards; `SpeculativeWave` is retained only as a scheduler
level/diagnostic container because candidate scheduling still reports levels.

The MiniWarehouse report now measures MVCC launch-mask cost, version publication, worker readiness,
contract/receipt work and worker concurrency. It explicitly reports zero per-transaction full-world
deep copies and zero historical write-set replays for the dependency executor.

### TODO before Phase 5D

- **Validate MVCC performance on real MiniWarehouse Wasm.** Re-run the 16-warehouse/B200 W1/W2/W4/W8
  diagnostic sweep first, then the block-size/contention sweep if correctness/reuse remain stable.
- **Investigate remaining Wasm concurrency inflation.** After state reconstruction is removed,
  compare aggregate contract/receipt cost against serial transaction work as worker count rises.
- **Investigate residual `Unknown` predicates.** Target graph precision and parallel exposure only
  after the execution-efficiency numbers are stable.
- **Parallel post-consensus validation/replay.** Keep concrete receipt dependencies as the
  correctness authority.
- **Snapshot-aware planning metadata.** Generalize N+1 planning overlap for contract create/remove.
- **Partial deadline completion.** Preserve receipts completed by the decision deadline when
  deadline enforcement is disabled.
- **Only then broaden Phase 5D learning policy.** The hard-to-soft loop remains intentionally
  narrow until this execution model has stable correctness and performance numbers.
