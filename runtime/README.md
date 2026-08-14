# Runtime

The runtime workspace contains the execution/evaluation side of ACG.

## Main crates

- `acg-cosmwasm-engine`: CosmWasm/native execution, receipts, validation, MVCC and READY-DAG metrics.
- `acg-cosmwasm-adapter`: converts execution requests into graph-facing transactions.
- `acg-runtime-feedback`: adaptive planning plus conflict/replay/serialization feedback.
- `acg-validator-sim`: deterministic ingress, mempool, block production and executors.
- `acg-evaluation`: stable experiment records, manifests and Brick-5F acceptance.
- `acg-benchmark-harness`: workload-independent serial/speculative benchmark pipeline.
- `acg-miniwarehouse-workload`: MiniWarehouse generator/adapter support.

## Test

```bash
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
```

Focused gates:

```bash
./scripts/run-brick5d-closed-loop-diagnostics.sh
./scripts/run-brick5e-measurement-diagnostics.sh
./scripts/run-brick5f-acceptance-diagnostics.sh
./scripts/run-common-benchmark-harness-diagnostics.sh
./scripts/run-control-plane-corrections-diagnostics.sh
```

Runtime execution semantics are stable while evaluation/tuning proceeds. VM lifecycle experiments
are not part of production runtime configuration.
