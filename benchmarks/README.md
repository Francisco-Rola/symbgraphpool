# Benchmarks

This workspace contains first-party CosmWasm contracts used to validate and tune ACG.

## Workloads

- `conflictlab`: controlled conflict/transaction-complexity workload.
- `miniwarehouse`: larger TPC-C-inspired application workload.

Symbolic analyzer output lives in `benchmarks/symbolic/` and is compiled against the runtime code
checksum by the harness.

## Build/test contracts

```bash
cargo test --manifest-path benchmarks/Cargo.toml --workspace
cargo build --manifest-path benchmarks/Cargo.toml \
  -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown
cargo build --manifest-path benchmarks/Cargo.toml \
  -p acg-benchmark-miniwarehouse --release --target wasm32-unknown-unknown
```

ConflictLab release evaluation uses the real Wasm artifact and supports compute, repeated-storage,
and payload-size complexity controls. See `evaluation/conflictlab/README.md`.
