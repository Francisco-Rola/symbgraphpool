# Tuning knobs

This file is the current tuning surface for experiments. Parameters recorded in a manifest are part
of the exact Brick-5F run identity.

## Execution mode

| knob | values | meaning |
|---|---|---|
| `mode` | `static`, `probability-only`, `cost-aware` | policy ablation; executor/validation path is identical |
| `workers` | `1..physical_core_limit` | READY-DAG execution workers |

## ACG scheduling / learning (`acg.*`)

| parameter | default | meaning |
|---|---:|---|
| `acg.edge_materialization_threshold` | `0.05` | posterior floor for unresolved static edges |
| `acg.soft_threshold` | `0.20` | risk at which an ordinary edge becomes soft |
| `acg.hard_threshold` | `0.80` | risk at which an evidence-mature edge is hard |
| `acg.risk_budget` | `0.20` | maximum same-wave aggregate soft risk |
| `acg.max_wave_width` | none | optional reported-wave width cap |
| `acg.independent_observations_before_softening` | `8` | independence samples before initially-hard edges may soften |
| `acg.serialization_cost_reference_nanos` | `250000` | cold-start serialization-cost fallback before 5E learns an edge cost |
| `acg.invalidation_fanout_weight` | `0.50` | replay penalty added per expected invalidated descendant |
| `acg.feedback_retention_factor` | `0.99` | epoch-to-epoch evidence retention; lower forgets faster |
| `acg.feedback_confidence_scale` | `20.0` | evidence weight needed to approach full confidence |
| `acg.fallback_prior_probability` | `0.50` | prior conflict probability for runtime-discovered topology |
| `acg.fallback_prior_strength` | `2.0` | strength of that fallback prior |
| `acg.feedback_epsilon` | `0.25` | numerical/evidence floor used by adaptive feedback |
| `acg.include_reverted_accesses` | `false` | include reverted accesses when deriving concrete conflicts |

Do not tune these before establishing the default-policy baseline. The release suite keeps policy
tuning in a separate matrix for that reason.

## Admission / block-production simulation (`sim.*`)

ConflictLab currently exposes these through the common harness:

| parameter | default | meaning |
|---|---:|---|
| `sim.admission_tps` | `25000` | deterministic virtual transaction admission rate into the mempool |
| `sim.block_interval_ms` | `2000` | virtual block-production / consensus-window length |
| `sim.block_size` | `transactions` | maximum transactions selected into the block |
| `sim.mempool_policy` | `fifo` | `fifo`, `reverse-fifo`, or deterministic `seeded-shuffle` order of the selected FIFO prefix |

For the current one-block harness window, actual block size is bounded by offered transactions,
admissions that fit in the block interval, and `sim.block_size`. Warm-up/measured windows are
deterministic and do not yet model long-lived backlog carry-over.

`reverse-fifo` and `seeded-shuffle` perturb canonical block order. **They do not yet perturb the
pre-consensus prediction itself** because the common harness starts execution from an already
produced block. A future prediction-policy/noise knob must explicitly compare a predicted mempool
prefix with a different produced block; do not describe current policy experiments as prediction
error experiments.

### Planned admission/block knobs

Not implemented yet:

- admission rejection/fee/priority policies (the mempool currently accepts every admitted tx);
- persistent multi-window backlog/queueing latency metrics;
- separate `prediction_policy` versus actual `mempool_policy`;
- controlled prediction noise / transaction replacement / reorder percentage;
- fee-, sender-, dependency-, or graph-aware block packing.

These are the knobs needed to study how imperfect block prediction affects 5C.5 pre-consensus work.

## ConflictLab workload knobs

| parameter | default | meaning |
|---|---:|---|
| `execution_backend` | `native` | `native` for fast harness tests; **`wasm` for performance evaluation** |
| `prediction_quality` | `exact` | `exact`, `coarse`, or `opaque`; controls how much of the actual key is visible to candidate construction |
| `transactions` | `200` | transactions offered during one workload window |
| `warmup_blocks` | `0` | deterministic adaptive-history blocks before the measured block |
| `accounts` | `16` | account-key cardinality |
| `hot_account_probability_bps` | `0` | probability (0–10000 bps) of choosing account 0 |
| `work_iterations` | `0` | deterministic contract compute iterations |
| `storage_rounds` | `0` | semantically neutral repeated read/write rounds on the same account key |
| `payload_bytes` | `0` | deterministic message payload bytes processed by the contract |
| `complexity` | label only | grouping label recorded in results; explicit work knobs define the actual cost |
| `warmup_hot_account_probability_bps` | measured value | warm-up contention level |
| `warmup_work_iterations` | measured value | warm-up compute cost |
| `warmup_storage_rounds` | measured value | warm-up storage intensity |
| `warmup_payload_bytes` | measured value | warm-up payload size |

Release ConflictLab matrices define five calibration tiers in `evaluation/conflictlab/README.md`.
They isolate transaction granularity while preserving the same account-level conflict relation.

Prediction-quality modes are controlled experiments, not claims about analyzer error rates:

- `exact`: the public `account` field is the real storage key and can be refined precisely;
- `coarse`: the profile keeps the account-key family but removes the concrete input dependency, so
  candidate predicates remain unresolved and can become soft;
- `opaque`: the public account is a unique decoy while the real key is routed through an opaque
  payload marker inside Wasm. This deliberately creates candidate misses so runtime fallback/replay
  learning can be evaluated.

For forced-speculation experiments, `acg.hard_threshold=0.95` and a larger risk budget are used to
allow unresolved candidate relationships to overlap initially. These settings are calibration tools,
not recommended production defaults.

## Control-plane / evaluation metrics

ExperimentRecord schema v2 keeps the original metrics and adds:

- `scheduling.pre_reduction_dependencies`: dependencies before exact hard-DAG transitive reduction;
- `scheduling.scheduled_dependencies`: dependencies actually handed to READY-DAG;
- `scheduling.edges_elided_by_reduction`: exact reachability-preserving hard edges removed;
- `feedback.observation_batches_applied`: learned profile relationships mutated after batching;
- `feedback.serialization_cost_batches_applied`: serialization-cost relationships mutated;
- `parallelism.observed_service_work_nanos`: aggregate observed speculative service work;
- `parallelism.worker_capacity_bound_nanos`: aggregate service divided by worker count;
- `parallelism.parallel_lower_bound_nanos`: max(observed critical path, worker-capacity bound);
- `parallelism.scheduler_realization_corrected_milli`: actual READY-DAG wall divided by that feasible
  lower bound.

The legacy DAG-only scheduler-realization field remains in schema v2 for comparison with older data.
Raw feedback observation counts are preserved even though updates are batched. Probability/replay
observations are aggregated once per persisted learned relationship and epoch within each feedback
phase; pre-execution and post-consensus phases remain separate because the split-phase pipeline can
consume pre-execution evidence before reconciliation completes. The persisted learner is currently
profile-edge/runtime-pair scoped, so this optimization does not invent a new clause-specific
posterior/checkpoint format.

## Lower-level engine knobs

These exist in `EngineConfig` but are intentionally not part of the common tuning matrix yet:

- gas limit: `10_000_000_000_000`;
- maximum nested call depth: `32`;
- contract-address prefix;
- Wasm memory-cache size: `128 MiB`;
- Wasm instance memory limit: `64 MiB`;
- pin-on-upload: enabled;
- available Wasm capabilities: currently `iterator`.

Change these only in dedicated engine experiments; adding them to a general matrix before there is a
research question would multiply the design space without improving scheduler conclusions.
