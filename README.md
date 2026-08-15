# Adaptive Conflict Graph

Adaptive Conflict Graph (ACG) is a Rust/CosmWasm research runtime for profile-guided speculative
transaction execution. Static symbolic conflicts are refined with concrete inputs, executed through
a READY-DAG + block-local MVCC engine, validated in canonical order, selectively replayed when
necessary, and fed back into a cost-aware scheduling model.

## Current system

- Bricks 1–4: symbolic profiles, concrete candidate graphs, runtime feedback, adaptive risk scheduling.
- Bricks 5A–5C.7: speculative receipts, canonical validation/replay, READY-DAG execution and MVCC.
- Bricks 5D–5E: replay/serialization-cost learning and stable experiment records.
- Brick 5F: manifest-driven correctness/provenance/acceptance gates.
- Common benchmark harness: deterministic serial reference + static/probability-only/cost-aware runs.
- Evaluation control plane: batched feedback, reachability-preserving hard-DAG reduction, worker-capacity-aware scheduler bounds, and ConflictLab prediction-quality calibration.

VM pooling/reset/cache-shard experiments are research-only under `research/vm-lifecycle/`.

## Build and test

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
```

For the complete checkout gate:

```bash
./scripts/run-brick5f-system-acceptance.sh
```

## Evaluation

Run the harness smoke test:

```bash
./scripts/run-common-benchmark-harness-diagnostics.sh
```

Run the focused control-plane/adaptation evaluation first:

```bash
./scripts/run-control-plane-corrections-diagnostics.sh
./scripts/run-conflictlab-control-plane-evaluation.sh
```

Then run the larger release-mode ConflictLab suite:

```bash
./scripts/run-conflictlab-release-suite.sh quick
# later: core or full
```

The suite builds the real ConflictLab Wasm contract, generates Brick-5F manifests, emits accepted
`records.jsonl`, and exports plot-ready CSV statistics.

See:

- `evaluation/README.md` — experiment workflow;
- `evaluation/conflictlab/README.md` — ConflictLab release matrices;
- `docs/tuning-knobs.md` — every current tuning parameter and planned block-prediction knobs;
- `docs/implementation-status.md` — implementation checkpoint;
- `runtime/README.md` — runtime crates and validation.

## Repository map

```text
crates/                 symbolic/core graph + feedback libraries
runtime/                execution engine, adaptive pipeline, evaluation and benchmark harness
benchmarks/             first-party CosmWasm contracts + symbolic profiles
evaluation/             manifests, matrix definitions and evaluation policies
scripts/                validation, sweep generation and aggregation commands
docs/                   architecture/status/tuning notes
research/                quarantined research artifacts
```

## Phase 6 consensus-realism evaluation

The consensus-cutoff, buffered serial pre-execution and candidate/decided-block divergence campaign
is documented in `evaluation/conflictlab/phase6-consensus-realism.md`.

Use `./scripts/run-all-tests.sh` after every patch. It formats and tests all Cargo workspaces and
runs the repository's shell/Python/evaluation-tool checks. Run
`./scripts/run-conflictlab-phase6-evaluation.sh` separately for the 952-run real-Wasm evaluation.

## ConflictLab 1.0 submission experimental suite

The frozen internal EuroSys/OSDI evidence suite is documented in
`evaluation/conflictlab/v1-experimental-suite.md`. It expands ConflictLab beyond the original point
credit workload to cover stateful point operations, ranges/deletes, bank reads/writes and queries,
contract creation, compact-vs-dense reference execution, symbolic-granularity ablations, controlled
prediction faults, workload transitions, binding consensus cutoffs, candidate/decided divergence,
fixed-hardware block scaling, statistical repetitions, and a long-history soak.

Run the repository gate before any long evaluation:

```bash
./scripts/run-all-tests.sh
```

Then run the complete 4,630-record real-Wasm suite:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

The suite intentionally fixes ACG to six workers on the current six-physical-core evaluation machine;
core-count and memory-capacity scaling are deferred. Paper-facing reports distinguish measured
post-consensus validation latency, the `max(pre, post)` **phase-bottleneck** metric, measured
non-overlapped sequential block time, and a hindsight concrete-conflict lower bound. It does not claim
or implement cross-block execution overlap. The V1 runner is resumable when invoked again with the
same output directory: fully accepted campaigns are reused only after an exact manifest/record
identity check, while incomplete or stale campaigns are rerun from a clean campaign directory.
