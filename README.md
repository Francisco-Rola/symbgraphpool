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
