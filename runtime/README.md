# Adaptive Conflict Graph runtime

This nested workspace contains the execution and single-validator simulation layers used by the
adaptive conflict-graph project. It is separate from the graph workspace because CosmWasm VM and
Wasmer dependencies are large and have a different build cadence.

## Crates

### `acg-cosmwasm-engine`

A deterministic CosmWasm execution engine with:

- execution of real CosmWasm Wasm bytecode through `cosmwasm-vm`;
- native mock contracts for deterministic engine tests;
- code upload, SHA-256 checksums, contract metadata, and compiled-module caching;
- per-contract ordered key/value storage and range iteration;
- one transaction overlay spanning storage, balances, and contract creation;
- atomic top-level commit or rollback;
- native bank balances, sends, burns, and funds attached to contract calls;
- nested `WasmMsg::Execute` and `WasmMsg::Instantiate`;
- `ReplyOn::{Always, Success, Error, Never}` behavior;
- bank, raw Wasm, and smart Wasm queries;
- read-only query enforcement;
- storage and bank access traces, including reverted child accesses.

Wasm upload uses CosmWasm's `Cache`: bytecode is statically checked, compiled once, written to the
filesystem cache, and pinned in memory by default. Later instantiate, execute, query, and reply
calls create fresh instances from the cached compiled module instead of recompiling Wasm.

The default cache uses a process-local temporary directory. For repeatable long-running benchmarks,
set `EngineConfig::wasm_cache.base_dir` to a persistent validator-local directory. Cache metrics are
available through `CosmWasmEngine::wasm_cache_metrics()`.

### `acg-cosmwasm-adapter`

The Brick 2 bridge from concrete runtime transactions to the runtime-independent conflict graph.
It resolves the executing code checksum and entrypoint to `ProfileId`, assigns dense local
`InstanceId`s, extracts concrete input bindings, and adapts a `ProducedBlock` into
`CandidateTransaction` records. Ordinary top-level CosmWasm execute enums are supported by
default; contract-specific nested dispatch can provide another `ExecuteEntrypointDecoder`.

### `acg-runtime-feedback`

Brick 3 adapter from concrete `ExecutionOutcome` access traces and pair-specific validation/replay
events to `acg-feedback` observations. It detects exact storage/bank overlaps, scan/write conflicts,
explicit independence for tracked pairs, runtime topology misses, and owns a convenience
`RuntimeFeedbackEngine` for batched update/checkpoint flow.

### `acg-benchmark-harness`

Manifest-driven common benchmark runner layered above Brick 5F. It prepares independent serial and
speculative workload instances, enforces deterministic setup, executes static/probability-only/
cost-aware policy ablations through the same READY-DAG/canonical-replay substrate, derives serial
DAG references and correctness digests, writes Brick 5E JSONL records, and evaluates the complete
dataset through Brick 5F. ConflictLab is the first built-in adapter.

### `acg-evaluation`

Brick 5E workload-independent experiment records. It combines adaptive planning/scheduling metrics,
READY-DAG service/DAG bounds, VM/host/MVCC diagnostics, validation/replay statistics, feedback update
overhead, learned replay/serialization-cost evidence, and optional serial-equivalence digests into a
schema-versioned deterministic JSON/JSONL record. Timing is validator-local optimization evidence
only and never participates in canonical correctness.

### `acg-miniwarehouse-workload`

Brick 2.5 benchmark traffic source for MiniWarehouse. It emits concrete `ExecutionRequest` values
using deterministic pseudo-random generation, can bootstrap the configured warehouse state, keeps
per-district order IDs monotonic, tracks generated orders so Delivery requests reference previously generated orders, and exposes
remote-stock plus hot-warehouse contention controls. Generated requests plug directly into
`RateControlledIngress`.

### `acg-validator-sim`

A deterministic, single-validator harness with four independent layers:

1. `RateControlledIngress`: virtual-time transaction injection at a configurable TPS.
2. `Mempool`: accepts every submitted transaction and stores it in FIFO order.
3. `BlockProducer`: advances a configurable block window and selects transactions through a
   `BlockSelectionPolicy`; the current policy is FIFO.
4. `BlockScheduler` plus `SerialBlockExecutor`: scheduling and execution are separate extension
   points. The baseline scheduler creates one transaction per wave.

The requested block-window default is two seconds. The default ingress rate is 25,000 transactions
per second, chosen as a benchmark-oriented reference to Injective's published throughput figure;
it is only a workload-generator default and not a claim about admission guarantees.

The serial executor still rejects wide plans. Split-phase speculative execution instead uses a
dependency READY-DAG plus block-local MVCC: scheduler levels are diagnostic only, completed
predecessor deltas are published as versions, and concrete canonical validation/replay remains the
correctness boundary.

## Validate

Run from the repository root:

```bash
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

The tests cover the original execution semantics plus Brick 2 runtime adaptation:

- compiled Wasm pinning and repeated pinned-cache hits;
- all-accepting FIFO mempool admission;
- deterministic rate-controlled ingress;
- two-second FIFO block production;
- serial FIFO scheduling and a custom reverse-order scheduler plug-in;
- block execution that preserves order and continues after failed transactions;
- serial-executor rejection of wide plans plus dependency/MVCC speculative execution tests;
- the complete submit, produce, schedule, and execute pipeline;
- runtime code-checksum to `ProfileId` resolution;
- dense `InstanceId` assignment;
- execute-message binding extraction;
- end-to-end ConflictLab `ExecutionRequest` to candidate-graph construction;
- all MiniWarehouse execute variants and nested line binding extraction;
- local/remote MiniWarehouse stock conflict construction;
- deterministic MiniWarehouse bootstrap/workload generation and ingress integration;
- concrete access conflict detection and explicit negative-evidence semantics;
- decayed Beta updates, runtime fallback edges, validation/replay evidence, and checkpoint restore.

## Engine usage

```rust
use acg_cosmwasm_engine::{Address, BlockContext, CosmWasmEngine, TransactionId};
use cosmwasm_std::{to_json_binary, Coin};
use serde_json::json;

let engine = CosmWasmEngine::default();
engine.set_balance("alice", &[Coin::new(1_000_u128, "utest")])?;
let code_id = engine.upload_wasm(wasm_bytes)?;

let instantiated = engine.instantiate(
    TransactionId(1),
    BlockContext::default(),
    Address::from("alice"),
    code_id,
    None,
    "example".to_owned(),
    vec![Coin::new(100_u128, "utest")],
    to_json_binary(&json!({ "owner": "alice" }))?,
)?;

println!("cache: {:?}", engine.wasm_cache_metrics());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Validator simulation usage

```rust
use acg_cosmwasm_engine::{CosmWasmEngine, ExecutionRequest};
use acg_validator_sim::{BlockProducerConfig, SingleValidatorRuntime};

let engine = CosmWasmEngine::default();
let mut validator = SingleValidatorRuntime::fifo(engine, BlockProducerConfig::default())?;

let request: ExecutionRequest = todo!("construct an instantiate or execute request");
validator.submit(request, 0);
let report = validator.produce_and_execute()?;
println!("executed {} transactions", report.transactions.len());
# Ok::<(), Box<dyn std::error::Error>>(())
```

## MiniWarehouse workload usage

```rust
use acg_cosmwasm_engine::Address;
use acg_miniwarehouse_workload::{
    MiniWarehouseWorkloadConfig, MiniWarehouseWorkloadGenerator,
};
use acg_validator_sim::{IngressConfig, Mempool, RateControlledIngress};

let config = MiniWarehouseWorkloadConfig::for_contract(Address::new("miniwarehouse"));
let mut generator = MiniWarehouseWorkloadGenerator::new(config)?;
let requests = generator.generate_requests(1_000)?;

let mut ingress = RateControlledIngress::new(
    IngressConfig { transactions_per_second: 500 },
    0,
)?;
ingress.enqueue_all(requests);
let mempool = Mempool::default();
ingress.pump_until(2_000_000_000, &mempool);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Call `generator.bootstrap()` first when the actual MiniWarehouse contract state has not already been
seeded.

## Runtime feedback usage

`acg-runtime-feedback` converts concrete execution traces into adaptive profile-edge evidence. A
`RuntimeFeedbackEngine` owns the collector and statistics store:

```rust
use acg_feedback::AdaptiveFeedbackConfig;
use acg_runtime_feedback::{
    RuntimeFeedbackEngine, RuntimeFeedbackWeights, TraceConflictConfig,
};

let mut feedback = RuntimeFeedbackEngine::new(
    &profile_graph,
    0,
    TraceConflictConfig::default(),
    RuntimeFeedbackWeights::default(),
    AdaptiveFeedbackConfig::default(),
)?;

let summary = feedback.process_block(
    &profile_graph,
    &candidate_graph,
    &execution_report,
    block_height,
)?;

let checkpoint = feedback.checkpoint(&profile_graph)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Concrete conflicts absent from static topology create reviewable runtime fallback edges. Failed
top-level executions are currently excluded from negative evidence because the engine does not yet
return a top-level failure trace artifact. See [`../docs/brick-3.md`](../docs/brick-3.md). Brick 5D/5E extend the same store with replay-impact and learned serialization-cost statistics; see [`../docs/brick-5d.md`](../docs/brick-5d.md) and [`../docs/brick-5e.md`](../docs/brick-5e.md).


## Dependency policy

The uploaded experimental fork was based on CosmWasm `2.0.0-rc.1`. This runtime uses the published
CosmWasm `2.0.9` crates instead of vendoring the fork. Exact versions and Rust-1.75-compatible
transitive guards are declared in `runtime/Cargo.toml`. The guard set pins `base64ct`, `zeroize`,
`indexmap`, `clru`, `uuid`, `rayon`, `rayon-core`, and `backtrace`; these direct declarations exist
only to keep CosmWasm/Wasmer's broad transitive requirements compatible with Rust 1.75.

Generate and commit `runtime/Cargo.lock` after successful dependency resolution. The runtime is an
executable research component, so its complete dependency graph should remain locked.

## Current limitations

- committed world state is in memory;
- a persistent Wasm cache directory is validator-local and must not be shared concurrently by
  independent engine processes;
- block time is virtual and deterministic rather than wall-clock driven;
- direct `Mempool::admit` means the transaction has already arrived; timestamp filtering is handled
  by `RateControlledIngress` before admission;
- `SubMsg.gas_limit` is accepted but not independently metered;
- reply `gas_used` is currently reported as zero;
- contract addresses use deterministic transaction-local allocation rather than a chain-specific
  address derivation;
- address canonicalization is deterministic UTF-8 rather than Bech32 or chain-specific bytes.
- MiniWarehouse workload generation tracks structural order sequencing but not committed stock/customer
  state; long runs should provision sufficient stock or Restock traffic until runtime feedback is wired
  into workload generation.

## Deliberately excluded

- actual peer-to-peer networking or mempool gossip;
- consensus voting, proposer election, finality, or forks;
- account signatures, staking, governance, and IBC;
- chain-specific protobuf messages and custom modules;
- speculative parallel commit, MVCC validation, and replay;
- durable state-database persistence;
- independent `SubMsg.gas_limit` enforcement;
- migration, admin updates, `Instantiate2`, and custom messages.
