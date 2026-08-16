# Benchmarks

First-party CosmWasm workloads used by the common benchmark harness:

- `conflictlab` — controlled conflict, prediction, execution-cost and runtime-semantics workload;
- `miniwarehouse` — larger TPC-C-inspired application workload.

Symbolic analyzer output lives in `benchmarks/symbolic/` and is compiled against the runtime code
checksum by the harness.

Build the contracts with:

```bash
cargo test --manifest-path benchmarks/Cargo.toml --workspace
cargo build --manifest-path benchmarks/Cargo.toml -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown
cargo build --manifest-path benchmarks/Cargo.toml -p acg-benchmark-miniwarehouse --release --target wasm32-unknown-unknown
```

See `evaluation/conflictlab/README.md` for the active ConflictLab experiments.
