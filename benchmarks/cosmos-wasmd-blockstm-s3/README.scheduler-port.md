# Wasmd scheduler-port patch

This harness evaluates four publication schedulers over the same S3 Wasm artifacts, Wasmd/WasmVM application state, Cosmos SDK keepers, deterministic compute calibration, and per-block direct-serial state oracle:

1. `cosmos-wasmd-direct-serial`
2. `cosmos-wasmd-block-stm`
3. `cosmos-wasmd-symbgraph-rust`
4. `cosmos-wasmd-vegeta`

## SymbGraph implementation

The publication SymbGraph row no longer reimplements symbolic scheduling in Go. `cosmos-wasmd-symbgraph-rust` sends block candidates to `runtime/crates/acg-wasmd-scheduler-ffi`, which directly uses the repository's authoritative `crates/acg-*` parser, profile graph, predicates, candidate graph, adaptive feedback, cost policy, and `RiskBoundedScheduler`.

The default symbolic profile directory remains:

```text
benchmarks/symbolic/native-s3/
```

Override it only when intentionally evaluating another frozen analysis set:

```bash
--symbolic-dir path/to/profiles
```

or:

```bash
VEGETA_S3_SYMBOLIC_DIR=path/to/profiles
```

The legacy Go `SymbGraphStaticRunner` and Go symbolic predictor are retained for explicit historical/diagnostic profiling only. They are not a fallback for the publication row.

See [`README.rust-symbgraph-bridge.md`](README.rust-symbgraph-bridge.md) for the language boundary, dependency-ready execution, launch-time visibility, canonical validation/replay, adaptive feedback, build procedure, and integration tests.

## Runner semantics

- Direct serial is the per-block correctness and timing control.
- Block-STM uses the SDK `txnrunner.STMRunner` unchanged.
- SymbGraph Rust executes the Rust scheduler's `ordering_dependencies` as a ready DAG; scheduler levels are diagnostic only.
- Go preserves the Phase-5 launch-visibility rule on private Cosmos cache branches, then uses actual KV/object/range fingerprints for canonical validation and replay.
- Vegeta speculates block transactions from the block-start state and performs deterministic validation/replay in source transaction order.
- SymbGraph Rust and Vegeta execute the real Wasmd transactions and use SDK `CacheMultiStore` branches.
- Reads made in discarded nested cache contexts still participate in validation, which is required for reverted S3 call scopes.
- Every scheduler's committed state digest is compared with direct serial after every block; a mismatch aborts the run.

The existing optional profiling passes remain separate replays and are not included in publication timing records.
