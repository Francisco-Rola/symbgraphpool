# Wasmd scheduler-port patch

This port evaluates four schedulers over the same S3 Wasm artifacts, Wasmd/WasmVM
application state, Cosmos SDK keepers, deterministic compute calibration, and
per-block direct-serial state oracle:

1. `cosmos-wasmd-direct-serial`
2. `cosmos-wasmd-block-stm`
3. `cosmos-wasmd-symbgraph-static`
4. `cosmos-wasmd-vegeta`

## SymbGraph symbolic prediction input

`cosmos-wasmd-symbgraph-static` consumes the repository's production source-derived
S3 symbolic profiles directly. The default is:

```text
benchmarks/symbolic/native-s3/
```

This is the same profile directory used by
`acg-vegeta-native-s3-benchmark --symbolic-dir benchmarks/symbolic/native-s3`.
Override it only when intentionally evaluating another frozen analysis set:

```bash
--symbolic-dir path/to/profiles
```

or:

```bash
VEGETA_S3_SYMBOLIC_DIR=path/to/profiles
```

The Go predictor mirrors the native Rust scheduler's static visibility rules:

- load each `contract` / `profiles[]` symbolic document;
- normalize entrypoints by retaining alphanumeric characters and lower-casing;
- select `kind::message_action`, e.g. `execute::Transfer`;
- concretize `depends_on.origin_input` from public call inputs and `info.sender`;
- unresolved dynamic keys become `*` wildcards;
- tuple dependencies become `a|b` composite keys;
- include native bank-send and execute-funds dependencies;
- create a pairwise edge whenever scope/resource/key overlap and at least one
  predicted access is a write.

Historical Ethereum trace keys are not predictor inputs. Correctness is checked
against actual Wasmd/Cosmos reads and writes and the direct-serial state digest.

## Runner semantics

* Direct serial is the per-block correctness and timing control.
* Block-STM uses the SDK `txnrunner.STMRunner` unchanged.
* SymbGraph static uses source-derived symbolic predictions to form pre-execution
  waves, then executes the real Wasmd transactions.
* Vegeta speculates block transactions from the block-start state and performs
  deterministic validation/replay in source transaction order.
* SymbGraph and Vegeta use SDK `CacheMultiStore` branches and track actual KV,
  object-store, and iterator-range accesses.
* Reads made in discarded nested cache contexts still participate in validation,
  which is required for reverted S3 call scopes.
* Every scheduler's committed state digest is compared with direct serial after
  every block; a mismatch aborts the run.

The existing optional 4-worker Block-STM pprof pass remains a separate replay and
is not included in publication timing records.
