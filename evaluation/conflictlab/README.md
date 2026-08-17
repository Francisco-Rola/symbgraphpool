# ConflictLab

ConflictLab is ACG's controlled ground-truth workload. It runs the real CosmWasm contract and is
used to isolate contention, prediction quality, scheduling policy, consensus timing, execution
semantics, and runtime overhead before moving to external workloads.

## ConflictLab 1.0

Run the full 4,630-record suite with:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

The V1 grids cover:

- static, probability-only, and cost-aware policies;
- low/high contention and block sizes up to 2,048 transactions;
- exact/bucketed prediction and prediction-fault recovery;
- fine/resource/profile symbolic granularity;
- compact-vs-dense candidate representation;
- consensus cutoff and candidate/decided-block divergence;
- non-stationary workload transitions;
- point, range/delete, bank, query, contract-create, and state-derived execution semantics;
- VM lifecycle controls, ordering sensitivity, risk tuning, statistical repetitions, and long soak.

All V1 paper-facing runs use six workers. Core-count and memory-capacity scaling are separate future
experiments.

## Focused post-V1 fixes

Before rerunning the full suite after adaptive/runtime changes, run the 146-record focused gate:

```bash
./scripts/run-conflictlab-fix-validation.sh
```

It isolates four behaviors: reordered-receipt read-set reuse, fail-safe regime changes with direct
serial probation/re-probes, temporary hidden-key miss recovery, and the cost-aware combined-pipeline
objective. The regime microbenchmark uses five seeds; direct-bypass correctness is checked structurally,
while bypass-vs-serial timing is reported only as a noise-sensitive diagnostic. All focused records still
require serial equivalence.

## Controlled parallelism ceiling

Run:

```bash
./scripts/run-conflictlab-parallelism-evaluation.sh
```

`parallelism-ceiling.grid.json` creates deterministic conflict lanes with `parallelism_lanes`:

- `1/2/3/4/6` lanes create that many balanced serial chains;
- `384` lanes with a 384-transaction block makes every transaction independent;
- six workers cap nominal hardware parallelism at 6x.

The experiment uses exact prediction, a non-binding consensus cutoff, no serial bypass, and
compute-heavy credit transactions. The nominal lane ceiling is structural; the hindsight oracle is
computed from measured serial per-transaction costs plus concrete conflicts and the six-worker
capacity bound. Because parallel execution can change service cost, the report also separates
service-time inflation/deflation from scheduler overhead. It reports:

- nominal lane/hardware ceiling;
- hindsight concrete-conflict oracle speedup;
- obtained worker-executor speedup and oracle efficiency;
- phase-bottleneck and sequential speedup;
- planning, dependency setup, executor gap, reconciliation, and feedback wall time;
- nested per-transaction Wasm/host/MVCC timing to localize overhead.

The lane sweep uses heavy transactions to test 1x→6x scaling. Additional 6-lane and fully-independent
cases vary compute cost to show how fixed overhead amortizes.

## Important workload controls

- `prediction_quality=exact|bucketed|coarse|opaque`
- `symbolic_granularity=fine|resource|profile`
- `operation_mix=credit|point-mixed|stateful-mixed|range-delete|bank-funds|bank-mixed|instantiate|full`
- `work_iterations`, `storage_rounds`, `payload_bytes`
- `hot_account_probability_bps`, `accounts`, `transactions`
- `parallelism_lanes` (0 = normal random/hot-account generator; positive = deterministic lanes)

Historical pre-V1 matrices remain in this directory for provenance but are not active entrypoints.
