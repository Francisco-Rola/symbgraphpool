# Wasmd scheduler-port patch

This harness evaluates five deployable/baseline schedulers plus one evaluation-only Rust-ACG perfect-access oracle over the same S3 Wasm artifacts, Wasmd/WasmVM application state, Cosmos SDK keepers, deterministic compute calibration, and per-block direct-serial state oracle:

1. `cosmos-wasmd-direct-serial`
2. `cosmos-wasmd-symbgraph-rust-exact-trace-oracle` — evaluation-only perfect symbolic-access upper bound for Rust-ACG
3. `cosmos-wasmd-block-stm`
4. `cosmos-wasmd-aria-fb`
5. `cosmos-wasmd-vegeta`
6. `cosmos-wasmd-symbgraph-rust`

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

The exact-trace oracle uses the frozen imported Ethereum SLOAD/SSTORE trace directory:

```text
benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces/
```

Override it only when intentionally evaluating another exact source-access set:

```bash
--exact-trace-dir path/to/tx-traces
```

or:

```bash
VEGETA_S3_EXACT_TRACE_DIR=path/to/tx-traces
```

Because the native CosmWasm translation is not a one-to-one storage-key encoding of EVM state, the oracle also requires the frozen native translation access audit produced during the native-execution preparation stage:

```text
benchmarks/corpora/vegeta-ethereum/s3/native-execution/native-accesses.jsonl
```

Override it with `--exact-native-accesses` or `VEGETA_S3_EXACT_NATIVE_ACCESSES`. This file is **not** collected by the scheduler campaign and is not timed. It is used only to add native RAW dependencies that are absent from the exact Ethereum relation because of translation aliasing/read-modify-write semantics.

The legacy Go `SymbGraphStaticRunner` and Go symbolic predictor are retained for explicit historical/diagnostic profiling only. They are not a fallback for the publication row.

See [`README.rust-symbgraph-bridge.md`](README.rust-symbgraph-bridge.md) for the language boundary, dependency-ready execution, launch-time visibility, canonical validation/replay, adaptive feedback, build procedure, and integration tests.

## Runner semantics

- Direct serial is the per-block correctness and timing control.
- `ACG-Oracle` is not a separate generic scheduler. Its primary access oracle is the frozen exact Ethereum transaction SLOAD/SSTORE set. Those accesses become the minimal hard RAW visibility dependencies required by ACG's canonical validator; pure source WAW/WAR pairs remain parallel because ACG commits deltas in canonical order. The Wasmd translation is not perfectly access-isomorphic to the EVM workload, so a **frozen pre-campaign native translation audit** contributes only additional RAW dependencies that exist after translation but not in the source relation. This compensation is required to avoid measuring translation artifacts as symbolic-analysis misses. Native adapter bank/funds resources remain hard exactly as in the normal ACG row.
- `ACG-Oracle` then runs the same Rust-ACG Wasmd ready-DAG execution, launch-time MVCC visibility, indexed canonical validation, delta reuse, and reconciliation path as the normal Rust-ACG row. Runtime adaptive feedback is disabled because the oracle already has perfect source accesses.
- `ACG-Oracle` is required to complete with **zero replay** and the historical-order direct-serial state digest. Any replay or state mismatch aborts the run instead of silently weakening the upper-bound claim.
- A source transaction whose exact imported trace is unavailable becomes a conservative serial barrier. The count is reported as `oracle_source_trace_missing`, so missing trace coverage can only make the oracle slightly pessimistic rather than optimistic.
- For source-failed/reverted transactions, trace writes are not treated as committed writes; their touched storage is retained conservatively as reads, matching discarded-write semantics.
- Exact source traces and the frozen translation-compensation audit are evaluation-only hindsight inputs. Neither is discovered during the measured scheduler campaign. The measured oracle row is intended to answer: “what would this same ACG implementation achieve if symbolic access analysis were perfect for the translated S3 workload?”
- Block-STM uses the SDK `txnrunner.STMRunner` unchanged.
- SymbGraph Rust executes the Rust scheduler's `ordering_dependencies` as a ready DAG; scheduler levels are diagnostic only.
- Go preserves the Phase-5 launch-visibility rule on private Cosmos cache branches, then uses actual KV/object/range fingerprints for canonical validation and replay.
- Vegeta ports the attached repository's `SpeculateMod`/`ParallelMod`: concrete pre-execution, hottest-key proposal reordering, upstream dependency precedence, Rule-2-compatible replay batches, and access-change/new-key handling.
- AriaFB ports the attached repository's Rule-2 abort test and transitively reduced hot-chain DAG fallback; Cosmos dynamic accesses have an additional conservative safety replay.
- SymbGraph Rust, ACG-Oracle, AriaFB, and Vegeta execute the real Wasmd transactions and use SDK `CacheMultiStore` branches.
- Reads made in discarded nested cache contexts still participate in validation, which is required for reverted S3 call scopes.
- Serial, Exact-Trace ACG-Oracle, BlockSTM, and Rust-ACG compare against historical-order direct serial. Vegeta and AriaFB compare against independent serial execution of their derived serialization order; `serial_reference_scope` records which oracle was used. Their `matched_serial_nanos` is timed on that same derived serial order, while `historical_serial_nanos` preserves the common original-order control. A mismatch aborts the run.

The summary additionally reports **Rust-ACG perfect-access headroom**. `oracle-tps / acg-tps` is the end-to-end fixed-consensus-window gain available if the current symbolic access extraction were perfect while the rest of ACG remained unchanged. `oracle-dag` and `acg-dag` show the corresponding cost-weighted planner work/span diagnostics. This is intentionally a comparison within the ACG design, not a generic conflict-DAG upper bound for all possible schedulers.

The existing optional profiling passes remain separate replays and are not included in publication timing records.
