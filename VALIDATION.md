# Validation status

## Completed in this environment

- Parsed the attached analyzer JSON with Python to inspect its actual shape.
- Confirmed 20 profiles, 2 declared storage resources, read/write accesses, four dependency kinds,
  and two delegation records.
- Independently reproduced delegation expansion and the Rust edge-derivation rules in Python.
- Expected fixture result after delegation composition: 66 profile edges, comprising 55 conditional
  and 11 unknown edges.
- Parsed every `Cargo.toml` with Python's TOML parser.
- Checked all repository JSON files with Python's JSON parser.
- Checked referenced fixture paths and repository file manifest.

## Not completed in this environment

A Rust toolchain and Cargo registry cache were not available, and outbound package downloads were
blocked. Therefore `cargo fmt`, `cargo test`, and `cargo clippy` were not executed here. The
repository includes CI commands and tests, but the first local or CI run should be treated as the
compiler verification step. Generate and commit `Cargo.lock` after that run.

## Brick 1 correction (2026-08-06)

The first external Rust 1.75 build exposed three `thiserror` derive failures. Fields named `source`
were being used as ordinary endpoint/entrypoint data, but `thiserror` reserves that field name for
an underlying error implementing `std::error::Error`. The corrected source renames them to
`source_entrypoint`, `source_profile`, and corresponding target names. The rustfmt changes reported
by the first external `cargo fmt --check` run have also been applied.

This environment still cannot execute Cargo, so the corrected archive must be verified with the
normal local sequence: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, and `cargo test --workspace --all-targets`.

## Benchmark resources (2026-08-06)

Added the nested `benchmarks/` Cargo workspace with the single-file ConflictLab and
MiniWarehouse CosmWasm contracts, analyzer-compatible symbolic JSON, parser tests, profile-graph
tests, CI coverage, and manual validation commands.

Static validation completed in this environment:

- parsed every new Cargo manifest with Python's TOML parser;
- parsed both symbolic analyzer documents with Python's JSON parser;
- checked that every evidence file and line interval points inside its contract source;
- checked balanced Rust delimiters while ignoring strings and comments;
- independently reproduced the current profile-edge derivation algorithm against both artifacts;
- obtained 18 profiles and 63 edges for ConflictLab: 46 conditional and 17 unknown;
- obtained 14 profiles and 44 edges for MiniWarehouse: 38 conditional and 6 unknown;
- confirmed ConflictLab delegation, wildcard balance access, and state-derived cancellation keys;
- confirmed MiniWarehouse variable-length stock/order-line accesses, prefix scans, and
  state-derived delivery customer keys.

The ConflictLab logical `CONFIG` fields are stored as independent `ADMIN`, `FEE_BPS`, and `EPOCH`
items. This keeps the field-level symbolic keys aligned with physical storage conflicts rather
than pretending independent fields inside one serialized singleton can be updated concurrently.

The environment still has no Rust toolchain or Cargo registry access. The benchmark contracts and
new Rust tests therefore require local or CI compiler verification with the commands in
`benchmarks/README.md`. Do not treat the static checks above as a substitute for `cargo fmt`,
`cargo clippy`, and `cargo test`.

## Benchmark MSRV correction (2026-08-06)

The first Rust 1.75 benchmark build allowed Cargo to select `base64ct` 1.8.3 through a broad
transitive `1.x` requirement. That release declares Rust 1.85 and uses Edition 2024, so Cargo 1.75
cannot parse it. The benchmark workspace now constrains `base64ct` to `1.6.0`, whose declared MSRV
is Rust 1.60, preserving the repository's Rust 1.75 policy. The rustfmt differences reported by the
external validation run have also been applied, and the graph-dump example is now source-controlled.

A subsequent Rust 1.75 resolution selected `zeroize` 1.9.0, which likewise declares Rust 1.85 and
Edition 2024. The benchmark workspace now also constrains `zeroize` to `1.8.2`, which declares Rust 1.60 and Edition 2021,
preserving Rust 1.75 support.

## Minimal CosmWasm execution runtime (2026-08-06)

Audited the uploaded `cosmosSE-master.zip` fork, concentrating on `packages/vm` and the fork-specific
interfaces it depended on in `packages/std` and `packages/crypto`. The audit and migration decision
are documented in `docs/audits/cosmosse-vm-audit.md`.

The new nested `runtime/` workspace uses published CosmWasm 2.0.9 and reimplements the useful
execution-layer concepts as host-side modules. It includes transactional contract storage, a bank
ledger, code and instance metadata, nested execute/instantiate/reply handling, smart and raw queries,
read-only query enforcement, concrete access traces, and a real-Wasm smoke fixture.

Static checks performed in this environment:

- parsed all runtime Cargo manifests with Python's TOML parser;
- decoded the Hackatom base64 fixture and verified the WebAssembly magic bytes;
- checked that no `todo!()` or `unimplemented!()` remains in runtime library code;
- checked repository-relative paths and third-party notices;
- checked the source tree and regenerated the file integrity manifest;
- reviewed rollback boundaries for top-level errors, child errors, replies, bank transfers, and
  contract creation;
- added explicit regression tests for self-transfer conservation, same-value query mutation,
  reply-data precedence, and nested contract creation;
- made contract address allocation transaction-local so failed submessages restore the instantiate
  ordinal together with all other transaction state.

A Rust toolchain and Cargo registry are still unavailable in this environment. Consequently the
runtime's first local or CI `cargo fmt`, `cargo clippy`, and `cargo test` run remains the definitive
compiler and VM-level validation step. Commit the generated `runtime/Cargo.lock` after that run.
## Runtime MSRV correction: indexmap (2026-08-06)

The first Rust 1.75 runtime resolution selected `indexmap` 2.14.0 through Wasmer's broad `2.x`
requirement. That release raises the MSRV to Rust 1.85 and uses Edition 2024, which Cargo 1.75 cannot
parse. The runtime workspace now constrains `indexmap` to `2.11.4`, whose declared MSRV is Rust 1.63
and whose manifest uses Edition 2021. The engine crate declares the workspace dependency directly so
Cargo unifies it with the transitive Wasmer requirement.

## Runtime MSRV correction bundle: clru and Wasmer transitive crates (2026-08-06)

After the `indexmap` correction, Cargo 1.75 selected `clru` 0.6.3 through CosmWasm VM's broad
`0.6.x` requirement. That release uses Edition 2024 and cannot be parsed by Cargo 1.75. The runtime
workspace now constrains `clru` to 0.6.2.

The same resolver pass had already selected several other releases with MSRVs above Rust 1.75:
`uuid` 1.24.0, `rayon` 1.12.0, `rayon-core` 1.13.0, and `backtrace` 0.3.76. To avoid exposing those
one at a time, the runtime now pins the mutually compatible guard set `uuid` 1.18.1, `rayon`
1.10.0, `rayon-core` 1.12.1, and `backtrace` 0.3.74. The engine crate declares each workspace
dependency directly so Cargo unifies it with the broad transitive CosmWasm/Wasmer requirement.

This environment still cannot run Cargo. The local acceptance sequence remains:

```text
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Runtime CosmWasm VM 2.0.9 backend signature correction (2026-08-06)

The first compiler pass reached the engine crate and reported that the `Storage::get` and
`Querier::query_raw` implementations used mutable receivers. In `cosmwasm-vm` 2.0.9 those
read-facing trait methods require `&self`. The engine already keeps transaction state behind a
shared mutex, so both adapters now use the required immutable receiver while retaining access
tracing and nested-query behavior through interior mutability.

The remaining storage operations that own iterator or write state continue to use `&mut self`.
The local acceptance sequence remains unchanged.

## Runtime simulation extension

Validate the compiled-module cache and validator simulator with:

```bash
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

New acceptance checks:

- Wasm upload leaves one compiled module in the pinned memory cache.
- Later instantiate and execute calls increment pinned-cache hits without cache misses.
- The mempool accepts duplicate transaction IDs and preserves admission order.
- Virtual-time ingress releases transactions at the configured rate.
- The default block window advances by exactly two seconds.
- FIFO selection preserves mempool order across block limits.
- The FIFO scheduler creates one transaction per wave.
- A custom scheduler can change execution order through the public scheduling trait.
- Failed transactions roll back and do not stop later block transactions.
- Parallel waves are rejected until speculative validation exists.

## Brick 2: concrete transaction graph (2026-08-10)

Added the runtime-independent `acg-predicate` and `acg-candidate-graph` crates plus the
`runtime/crates/acg-cosmwasm-adapter` bridge.

Brick 2 acceptance coverage now includes:

- dense validator-local `InstanceId` and block-local `TxIndex` domain types;
- concrete `CandidateTransaction` metadata validation;
- compilation of scalar, tuple, fixed-index, and synchronized symbolic-index key expressions;
- three-valued guard and key predicate evaluation (`true`, `false`, `unknown`);
- input-only guard pruning while state-dependent guards remain conservative;
- delegation-aware remapping of supported guard operands through analyzer input mappings;
- candidate transaction bucketing by `ProfileId` and persistent-adjacency-only traversal;
- same-profile pair generation without self-pairs or duplicate cartesian orientations;
- conservative materialization of unresolved/wildcard edges;
- CosmWasm code-checksum and entrypoint-selector to `ProfileId` resolution;
- dense contract-address to `InstanceId` resolution;
- pending-instantiate identity derived from the engine's deterministic predicted address;
- extraction of execute payload, `info.sender`, `info.funds`, and block context bindings;
- native selector override support;
- end-to-end ConflictLab `ExecutionRequest` -> `CandidateTransaction` -> `CandidateGraph` flow.

Eighteen new tests are included across the three Brick 2 crates: six predicate tests, seven
candidate-graph tests, two adapter unit tests, and three ConflictLab runtime integration tests.

Static validation completed in this environment:

- parsed every repository Cargo manifest with Python's TOML parser;
- checked Rust delimiter balance while ignoring comments and string literals;
- checked all new Rust source lines against the repository's 100-column formatting target;
- checked benchmark fixture paths used by the new integration tests;
- reviewed the candidate builder so each loaded profile edge is traversed once and cross-instance
  contract-local predicates can be pruned before materialization.

The environment still has no usable Rust/Cargo toolchain and outbound package resolution is
blocked. Therefore the definitive Brick 2 acceptance remains the local or CI sequence:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Brick 2.1: clause-level conflict resolution (2026-08-10)

Refined persistent profile-edge predicates so uncertainty is attached to individual alternative
clauses rather than represented only by the coarse edge-level relation.

Changes:

- added `ClauseResolution::{Conditional, Unconditional, Unknown}` to every persisted predicate
  clause;
- added persisted/dynamic `UnknownReason` diagnostics;
- retained `ProfileEdge.relation` only as summary metadata computed from clause resolutions;
- graph loading now rejects inconsistent relation summaries, unknown clauses without reasons, and
  resolved clauses that incorrectly carry unknown reasons;
- `CompiledPredicate::evaluate` continues to use allocation-free three-valued OR on the hot path;
- added `CompiledPredicate::evaluate_detailed` for clause-by-clause debugging and unknown-reason
  inspection;
- candidate graph construction still evaluates every clause even when the profile-edge summary is
  `Unknown`;
- bumped the portable profile-graph artifact format from version 1 to version 2. Existing analyzer
  JSON should be recompiled; legacy v1 graph artifacts are rejected explicitly.

Regression coverage includes ConflictLab `CreateOrder <-> CancelOrder`, which now preserves
input-resolvable `ORDERS` clauses separately from state-derived unresolved `BALANCES` clauses.
Synthetic mixed-clause tests validate `true OR unknown -> true`, `false OR unknown -> unknown`, and
`false OR false -> false`.

Additional tests also cover dynamic unknown reasons for missing bindings, unsupported expressions,
and state-dependent guards, plus graph-loader metadata validation and legacy artifact rejection.

The environment still has no Rust toolchain. Definitive acceptance remains:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Brick 2.5: MiniWarehouse structured integration (2026-08-10)

Added structured MiniWarehouse online-path coverage and the runtime `acg-miniwarehouse-workload`
crate.

Static validation completed in this environment:

- parsed the updated MiniWarehouse analyzer JSON and all Cargo manifests;
- independently reproduced the profile-edge derivation after order-line prefix refinement;
- obtained 14 profiles and 44 edges: 38 conditional and 6 unknown;
- confirmed the six remaining unknown profile relationships all involve Delivery's state-derived
  `CUSTOMERS` key;
- confirmed NewOrder `lines[i]` and StockLevel `item_ids[i]` expressions are represented as
  variable-length concrete key sets;
- confirmed the input-derived `(warehouse_id, district_id, order_id)` ORDER_LINES prefix is present
  on NewOrder, Delivery, and OrderStatus accesses;
- added deterministic workload tests for bootstrap coverage, operation generation, remote/local
  stock, hot-warehouse forcing, order/delivery sequencing, and virtual-time ingress;
- added candidate-graph tests for remote-stock overlaps, non-empty order guards, list-valued stock
  queries, warehouse-level Payment contention, and mixed Delivery diagnostics;
- added runtime adapter coverage for all eight MiniWarehouse execute variants and nested line
  bindings.
- added generator → runtime adapter → candidate-graph coverage proving a generated NewOrder creates
  an exact matching stock edge and prunes a nonmatching stock transaction.

The environment still has no Rust toolchain. Definitive acceptance remains:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Brick 3: runtime feedback and adaptive statistics (2026-08-10)

Added the runtime-independent `acg-feedback` crate and the runtime-specific
`runtime/crates/acg-runtime-feedback` bridge.

Brick 3 features:

- explicit positive conflict and negative independence observations;
- separate pre-execution, canonical execution, validation, and replay evidence sources;
- configurable source weights with post/canonical evidence stronger than speculative evidence;
- exact concrete storage and bank access indexing;
- read/write, write/read, and write/write conflict attribution;
- storage scan versus in-range write attribution;
- contract-local storage scope and global bank-key scope;
- reverted-access inclusion configurable for audit experiments;
- no negative evidence for untracked pairs or failed top-level transactions;
- decayed Beta-Bernoulli statistics with projected confidence at a requested epoch;
- per-block observation buffering and batched updates;
- static predicate-miss updates to existing profile edges;
- runtime topology misses creating reviewable fallback profile edges;
- future independence updates for already-discovered fallback pairs;
- stable-key-based versioned feedback checkpoints;
- restore of static and runtime-discovered statistics;
- pair-specific validation/invalidation/replay ingestion prepared for the speculative executor;
- `RuntimeFeedbackEngine` facade for collect -> apply -> checkpoint integration.

New focused tests: 9 `acg-feedback` tests, 15 `acg-runtime-feedback` tests, plus one profile-graph
lookup regression test. They cover posterior movement, decay, confidence projection, out-of-order
batch epochs, empty-buffer semantics, fallback creation/update/checkpointing, stale observation
rejection, exact storage and bank conflict scope, scan conflicts, reverted accesses, explicit
negative evidence, runtime topology misses, static predicate misses, failed-transaction handling,
fallback negative learning, pre/post weights, validation/replay events, transaction-id trace
validation, end-to-end feedback-engine checkpoint restore, and MiniWarehouse adaptation across conflicting/independent workload phases.

Static validation completed in this environment:

- parsed all Cargo manifests with Python's TOML parser;
- parsed all repository JSON artifacts;
- checked new Rust source delimiter balance while ignoring comments/string literals;
- checked all new benchmark fixture paths used by tests;
- reviewed that absence of an observation never becomes negative evidence;
- reviewed that static topology remains immutable while mutable statistics/fallbacks are separated;
- reviewed checkpoint identity so persistence uses `StableProfileKey` rather than dense `ProfileId`.

A Rust toolchain is still unavailable in this environment. The definitive acceptance commands are:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Brick 5E: learned serialization cost and stable experiment records (2026-08-13)

Brick 5E adds validator-local per-transaction READY-DAG timing, decayed marginal serialization-cost
statistics, feedback checkpoint v3 with v1/v2 restore compatibility, and the
`runtime/crates/acg-evaluation` schema-v1 JSON/JSONL experiment record.

Focused acceptance command:

```text
./scripts/run-brick5e-measurement-diagnostics.sh
```

The focused suite validates serialization-cost decay/persistence, learned serialize-vs-speculate
risk, deterministic record round-trips/schema rejection, and a real two-worker READY-DAG dependency
whose measured marginal delay is learned in one block and consumed by the next block's candidate
edge. The runner emits `records.jsonl` under `benchmark-results/brick5e-measurement/<timestamp>/`.

A Rust toolchain is unavailable in the artifact-construction environment. Definitive acceptance
remains local `cargo fmt`, workspace tests, and `cargo clippy -- -D warnings` for both root and
runtime workspaces, followed by the focused Brick 5E runner.


## Brick 5F: formal acceptance and reproducible evaluation gates (2026-08-13)

Brick 5F adds versioned experiment manifests and acceptance reports, publication/smoke acceptance
policies, serial-state SHA-256 digest helpers, best-effort provenance capture, core-budget and
internal-consistency validation, explicit optional CI performance thresholds, and the
`acg-evaluate` JSONL validator.

Focused acceptance command:

```text
./scripts/run-brick5f-acceptance-diagnostics.sh
```

Full source/test/lint gate:

```text
./scripts/run-brick5f-system-acceptance.sh
```

Generic result validation:

```text
./scripts/validate-experiment-records.sh <manifest.json> <records.jsonl> [acceptance.json]
```

The focused tests distinguish incomplete records, correctness failures, configuration errors, and
performance regressions; detect missing/unexpected/duplicate samples; round-trip manifest/report
JSON; validate state digests and JSONL loading; check metadata capture; and rerun the existing Brick
5E measurement/schema integration. Final Rust acceptance remains local formatting, workspace tests,
and Clippy with `-D warnings` for both root and runtime workspaces.

## Common benchmark harness (2026-08-13)

Added `runtime/crates/acg-benchmark-harness`, a manifest-driven workload adapter/runner layered above
Brick 5F. Every run independently prepares a serial reference and requested speculative ablation,
checks deterministic setup, executes warm-up plus one measured block, fills Brick 5E serial/DAG and
correctness fields, writes JSONL, and evaluates the complete dataset through Brick 5F.

Focused acceptance command:

```text
./scripts/run-common-benchmark-harness-diagnostics.sh
```

Generic manifest execution:

```text
./scripts/run-benchmark-manifest.sh <manifest.json> [output-directory]
```

The first built-in ConflictLab adapter supports static, probability-only, and cost-aware modes,
manifest-driven workload/scheduler/feedback knobs, and distinct warm-up versus measured contention
and work-cost parameters. Unknown workload or reserved `acg.*` parameters are rejected rather than
silently ignored.

Focused tests cover deterministic preparation, independent serial/adaptive state, serial-equivalence
digests, serial/DAG reference population, ablation-specific feedback separation, tuning-parameter
parsing, worker-budget enforcement, output-file round trips, and unknown workload handling. The
end-to-end smoke manifest executes all three policy modes and must be accepted by Brick 5F.

Static construction-environment validation completed with TOML/JSON parsing, shell `bash -n`,
lightweight changed-Rust delimiter checks, and patch whitespace/application checks. Definitive Rust
acceptance remains local `cargo fmt`, runtime workspace tests, and runtime workspace Clippy with
`-D warnings`, followed by the focused harness diagnostics runner.
