# Adaptive Conflict Graph

A modular Rust implementation of profile-guided smart-contract conflict graphs.

The repository currently implements:

- parsing and validating symbolic-analyzer JSON documents;
- normalization into runtime-independent symbolic profiles;
- deterministic stable profile keys;
- dense validator-local `ProfileId` assignment at graph load time;
- indexed derivation of offline profile edges from read/write resource overlap;
- a serializable graph artifact and an immutable in-memory CSR graph;
- a small compiler/inspection CLI;
- tests using Astroport and the source-controlled ConflictLab and MiniWarehouse workloads;
- a separate benchmark-contract workspace with analyzer-compatible symbolic profiles;
- a minimal CosmWasm execution runtime with compiled-module caching, atomic storage, native transfers, nested calls, and access tracing;
- a deterministic single-validator harness with rate-controlled ingress, an all-accepting FIFO mempool, two-second block windows, and scheduler/executor extension points;
- Brick 2 concrete-transaction adaptation, executable three-valued predicates, and candidate transaction-graph materialization;
- Brick 2.1 clause-level conflict resolution and explicit unknown-reason metadata;
- Brick 2.5 MiniWarehouse structured integration, input-derived order-line prefixes, and a deterministic workload generator;
- Brick 3 concrete execution feedback, decayed Beta statistics, fallback edges, and checkpoints;
- Brick 4 adaptive weighted candidate graphs and risk-bounded scheduling;
- Brick 5A/5B speculative receipts with canonical validation/reuse/replay;
- Brick 5C.7 dependency-driven READY-DAG pre-execution over block-local persistent MVCC;
- Brick 5D replay attribution, decayed replay-cost/fan-out feedback, and cost-adjusted scheduling risk.

## Repository layout

```text
crates/acg-core            Domain types, profile identities, symbolic profiles, edge types
crates/acg-symbolic-json   Analyzer JSON schema and normalization
crates/acg-profile-graph   Edge derivation, artifact format, dense-ID graph loader
crates/acg-predicate       Precompiled symbolic expressions and three-valued predicate evaluation
crates/acg-candidate-graph Concrete candidate transactions and per-block conflict graphs
crates/acg-feedback        Runtime observations, adaptive statistics, fallback edges, checkpoints
crates/acg-cli             `acg-profilec` compiler and inspector
benchmarks/contracts       Single-file CosmWasm benchmark contracts
benchmarks/symbolic        Analyzer-compatible symbolic profile JSON
benchmarks/README.md       Benchmark build, inspection, and debugging guide
runtime/                   CosmWasm engine, adapter, MiniWarehouse workload generator, and validator simulation
docs/audits/               Audit records for imported or replaced prototypes
```

The crates deliberately separate untrusted analyzer input from graph-runtime data. The online
path consumes a precompiled `ProfileGraphArtifact`; it does not parse source code or run symbolic
analysis.

## Build

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Benchmark resources

The repository includes two first-party workloads:

- **ConflictLab**: a compact debugging contract for exact key equality, conditional guards,
  delegation, state-derived keys, and wildcard accesses.
- **MiniWarehouse**: a non-compliant TPC-C-inspired integration workload with multi-record
  transactions and variable-size order lines.

The contracts use a separate Cargo workspace so CosmWasm dependencies do not enter the core graph
crates. Build and test them with:

```bash
cargo test --manifest-path benchmarks/Cargo.toml --workspace
```

Compile their symbolic profiles through the same CLI used for Astroport. Complete commands and
expected graph counts are in [`benchmarks/README.md`](benchmarks/README.md).

## Execution runtime

The `runtime/` workspace executes real CosmWasm bytecode and provides the host-side state model that
will later feed concrete read/write observations into the adaptive graph. It also contains a
deterministic single-validator simulation of transaction ingress, an all-accepting FIFO mempool,
configurable block windows, scheduling plans, and serial block execution. It deliberately excludes
real peer-to-peer networking, consensus voting/finality, staking, governance, and IBC.

```bash
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

See [`runtime/README.md`](runtime/README.md) for usage and
[`docs/audits/cosmosse-vm-audit.md`](docs/audits/cosmosse-vm-audit.md) for the audit of the uploaded
experimental fork.

## Compile analyzer output

The analyzer JSON does not contain the runtime identifier or contract code hash. They must be
provided by the caller because they are part of profile identity.

```bash
cargo run -p acg-cli -- compile \
  --input crates/acg-symbolic-json/tests/fixtures/astroport_pair_compact_config_fields.json \
  --output /tmp/astroport.profile-graph.json \
  --runtime cosmwasm \
  --code-hash 0000000000000000000000000000000000000000000000000000000000000001
```

Inspect the compiled artifact:

```bash
cargo run -p acg-cli -- inspect --input /tmp/astroport.profile-graph.json
```

## Identity policy

`StableProfileKey` is BLAKE3 over a domain-separated canonical binary encoding of:

```text
(runtime_id, contract_code_hash, entrypoint_kind, numeric_entrypoint_selector,
 profile_schema_version)
```

Analyzer entrypoint names are converted to numeric selectors using a domain-separated 64-bit
BLAKE3 digest. A chain-native numeric selector can override that fallback through
`IngestionContext::selector_overrides`.

`ProfileId` is a dense `u32`. It is never persisted as identity. At graph load, profile records are
sorted by `StableProfileKey` and assigned IDs `0..N`, making loading deterministic and enabling
array-based adjacency.

## Concrete transaction graph

The compiler still indexes normalized accesses by `(contract code hash, resource family, semantic
key component)` and excludes read-read profile pairs. Brick 2 now compiles each profile-edge
predicate into an executable representation and evaluates it against concrete transaction
bindings.

The candidate builder buckets transactions by dense `ProfileId`, traverses only persistent profile
adjacencies, and materializes concrete edges for `true` and `unknown` predicate results. A `false`
result is a concrete pruning decision. Brick 2.1 classifies each alternative predicate clause
independently as conditional, unconditional, or unknown; the edge relation is now only summary
metadata. Detailed predicate evaluation reports why an unresolved clause remains unknown. Contract-local clauses additionally compare dense
validator-local `InstanceId`s, so two deployments of the same code do not collide merely because
they share a profile.

The CosmWasm adapter derives these graph-facing transactions directly from the existing
`ExecutionRequest` type using runtime code checksums, contract addresses, execute-message payloads,
`info`, and block context. See [`docs/brick-2.md`](docs/brick-2.md), [`docs/brick-2.1.md`](docs/brick-2.1.md), and [`docs/brick-2.5.md`](docs/brick-2.5.md).

## Structured MiniWarehouse workload

Brick 2.5 adds a deterministic runtime workload generator for MiniWarehouse. It can bootstrap the
configured warehouse/district/customer/stock domain, generate repeatable NewOrder/Payment/Delivery/
Restock traffic, control remote-stock and hot-warehouse probabilities, and feed the existing
`RateControlledIngress`. MiniWarehouse ORDER_LINES conflict matching now uses the fully
input-derived `(warehouse_id, district_id, order_id)` prefix, reducing the static graph to 38
conditional and 6 unknown edges while retaining state-derived Delivery customer uncertainty. See
[`docs/brick-2.5.md`](docs/brick-2.5.md).

## Adaptive runtime feedback

Brick 3 adds a runtime-neutral feedback/statistics store plus a CosmWasm trace-attribution adapter.
Concrete storage/bank overlaps become positive observations; successful candidate-edge comparisons
without overlap become explicit negative observations. A decayed Beta posterior is maintained per
static edge, while concrete conflicts missing from the symbolic topology create persistent
reviewable fallback edges. Pre-execution, canonical execution, validation, and replay evidence have
separate configurable weights. Feedback checkpoints use stable profile keys so statistics survive
dense `ProfileId` reassignment. See [`docs/brick-3.md`](docs/brick-3.md).

Brick 5D extends that feedback with exact reconciliation causes, measured replay cost/fan-out, and a
separate cost-adjusted scheduling risk while keeping canonical validation/replay as the correctness
authority. See [`docs/brick-5d.md`](docs/brick-5d.md).


## Publication checklist

Before publishing, replace the placeholder repository URL, select project governance, add a code
of conduct, and document the analyzer JSON compatibility policy.

## Implementation checkpoint and research archives

Current production/research status is summarized in [`docs/implementation-status.md`](docs/implementation-status.md).
VM-pool, adaptive-preparation, acquisition-throttling, unsafe-retained-instance, and fresh-cache-shard experiments are intentionally excluded from the production engine and preserved under [`research/vm-lifecycle/`](research/vm-lifecycle/README.md).
