# Wasmd scheduler-port patch

This harness evaluates five publication schedulers over the same S3 Wasm artifacts, Wasmd/WasmVM application state, Cosmos SDK keepers, deterministic compute calibration, and per-block direct-serial state oracle:

1. `cosmos-wasmd-direct-serial`
2. `cosmos-wasmd-block-stm`
3. `cosmos-wasmd-aria-fb`
4. `cosmos-wasmd-vegeta`
5. `cosmos-wasmd-symbgraph-rust`

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
- Vegeta ports the attached repository's `SpeculateMod`/`ParallelMod`: concrete pre-execution, hottest-key proposal reordering, upstream dependency precedence, Rule-2-compatible replay batches, and access-change/new-key handling.
- AriaFB ports the attached repository's Rule-2 abort test and transitively reduced hot-chain DAG fallback; Cosmos dynamic accesses have an additional conservative safety replay.
- SymbGraph Rust, AriaFB, and Vegeta execute the real Wasmd transactions and use SDK `CacheMultiStore` branches.
- Reads made in discarded nested cache contexts still participate in validation, which is required for reverted S3 call scopes.
- Serial, BlockSTM, and Rust-ACG compare against historical-order direct serial. Vegeta and AriaFB compare against independent serial execution of their derived serialization order; `serial_reference_scope` records which oracle was used. Their `matched_serial_nanos` is timed on that same derived serial order, while `historical_serial_nanos` preserves the common original-order control. A mismatch aborts the run.

The existing optional profiling passes remain separate replays and are not included in publication timing records.
