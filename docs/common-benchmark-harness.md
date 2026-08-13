# Common benchmark harness

The common harness is the first layer after Brick 5F. It standardizes how workloads become accepted
research data without changing the READY-DAG executor or canonical validation/replay semantics.

## One run, one measured block

Each `ExperimentManifest.runs[]` entry denotes one independently reproducible measured block. A
workload adapter may generate deterministic warm-up blocks before it. The harness prepares the
workload twice from the same run identity:

```text
manifest run
   |
   +-- prepare serial reference -- warm-up -- measured block -- final state bytes
   |
   `-- prepare requested mode --- warm-up -- measured block -- final state bytes
                                      |
                                      +-- planning/scheduling/execution/replay/feedback metrics
                                      `-- Brick 5E ExperimentRecord

serial/adaptive initial setup must be byte-identical
serial/adaptive generated blocks must be structurally identical
final state digests must match
```

This keeps the frozen Brick 5E schema aligned with the Brick 5F requirement of exactly one record
per declared `RunIdentity`. Phase-history experiments encode their warm-up/history explicitly in the
run parameters rather than depending on hidden runner state.

## Workload adapter boundary

`acg-benchmark-harness` exposes two traits:

- `BenchmarkWorkload`: deterministic setup from a `RunIdentity`;
- `PreparedBenchmark`: engine, profile graph, warm-up/measured blocks, and deterministic canonical
  state encoding.

Adding a workload should require implementing those traits and registering the adapter. It should
not require modifications to READY-DAG, MVCC, validation, feedback, or the evaluation schema.

The first built-in adapter is ConflictLab. MiniWarehouse is the next adapter to add once the common
runner is locally validated.

## Ablation modes

The same execution/canonical-replay substrate supports three policy modes:

- `static`: plan from static/prior information and retain no execution feedback;
- `probability-only`: learn conflict/topology evidence, but deliberately exclude replay-cost,
  fan-out, and serialization-cost evidence;
- `cost-aware`: full Brick 5D/5E conflict probability, replay impact, and learned marginal
  serialization cost.

This makes static vs probability-only vs cost-aware comparisons an actual policy ablation rather
than three different executors.

A future oracle adapter/mode can be added for workloads such as ConflictLab where true concrete
conflicts are known, without changing these three baselines.

## Tuning parameters

Workload parameters are adapter-defined. ConflictLab currently accepts:

- `transactions`;
- `warmup_blocks`;
- `accounts`;
- `hot_account_probability_bps`;
- `work_iterations`;
- `warmup_hot_account_probability_bps`;
- `warmup_work_iterations`.

Keys beginning with `acg.` are common runtime tuning parameters interpreted by the harness:

- `acg.edge_materialization_threshold`;
- `acg.soft_threshold`;
- `acg.hard_threshold`;
- `acg.risk_budget`;
- `acg.max_wave_width` (`none`/`null` or an integer);
- `acg.independent_observations_before_softening`;
- `acg.serialization_cost_reference_nanos`;
- `acg.invalidation_fanout_weight`;
- `acg.feedback_retention_factor`;
- `acg.feedback_confidence_scale`;
- `acg.fallback_prior_probability`;
- `acg.fallback_prior_strength`;
- `acg.feedback_epsilon`;
- `acg.include_reverted_accesses`.

The exact parameter map remains in `ExperimentMetadata.parameters`, making tuning campaigns
self-identifying and compatible with Brick 5F's exact run-matrix matching.

## Serial reference and DAG references

For every measured speculative block the harness independently executes the same block canonically
and serially. Per-transaction serial service costs are then projected through the measured adaptive
schedule to fill:

- `serial_equivalent_work_nanos`;
- `serial_cost_dag_bound_nanos`.

The speculative execution itself supplies the observed-service DAG bound and actual READY-DAG wall.
The workload's deterministic canonical-state encoding supplies the serial/parallel SHA-256 digests.

## CLI

Run an arbitrary manifest with:

```bash
./scripts/run-benchmark-manifest.sh evaluation/conflictlab-harness-smoke.json
```

or directly:

```bash
cargo run \
  --manifest-path runtime/Cargo.toml \
  -p acg-benchmark-harness \
  --bin acg-benchmark \
  -- \
  manifest.json records.jsonl acceptance.json .
```

The binary exits with:

- `0`: all manifest runs executed and Brick 5F accepted the resulting dataset;
- `1`: execution completed but Brick 5F rejected the dataset;
- `2`: malformed manifest, unknown workload/mode, runtime failure, or I/O error.

## Validation

Focused tests and an end-to-end three-mode smoke manifest are run with:

```bash
./scripts/run-common-benchmark-harness-diagnostics.sh
```

The tests cover deterministic preparation, serial-equivalence/digests, static/probability/cost-aware
feedback separation, manifest-driven tuning values, worker-budget enforcement, JSONL/report file
creation, and unknown-workload rejection.
