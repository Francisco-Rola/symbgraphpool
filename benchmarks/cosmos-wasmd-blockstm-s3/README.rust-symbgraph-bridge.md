# Rust ACG -> Wasmd SymbGraph bridge

This benchmark now has one publication SymbGraph implementation: the scheduler/core state remains in the repository's Rust `crates/acg-*` stack and the Go Wasmd harness acts as its execution adapter.

The previous `SymbGraphStaticRunner` is intentionally retained as diagnostic/reference code. It is not the `cosmos-wasmd-symbgraph-rust` publication row.

## Ownership boundary

Rust remains authoritative for:

```text
acg-symbolic-json
    -> acg-profile-graph
    -> acg-predicate
    -> acg-candidate-graph
    -> acg-feedback
    -> RiskBoundedScheduler
    -> ordering_dependencies
```

The bridge does not port those algorithms to Go. `runtime/crates/acg-wasmd-scheduler-ffi` links the existing crates and exposes a block-granular C ABI:

```text
new -> plan -> feedback -> free
```

No `sdk.Context`, keeper, `MultiStore`, WasmVM pointer, or per-KV callback crosses the FFI boundary. JSON is used at the boundary because planning and feedback occur once per block, not once per store access.

Go remains authoritative only for things which are Go/Cosmos objects:

- real Wasmd/WasmVM execution;
- private Cosmos transaction cache branches;
- concrete SDK KV read/write fingerprinting;
- launch-time speculative visibility;
- canonical validation, receipt reuse, and replay;
- adapter-only Cosmos bank/funds resources which are outside contract symbolic storage.

The existing runtime crates are unchanged and remain the reference implementation for the execution semantics that the Go adapter follows.

## Planning

At bridge creation, Go sends every checked-in symbolic document to Rust as raw JSON. Rust runs `acg-symbolic-json` parsing/normalization, compiles the combined `ProfileGraph`, and creates the existing adaptive feedback store.

For each Wasmd block, Go converts one atomic transaction into zero or more ACG candidate components. Each execute/query contract call becomes a component with:

- contract family;
- concrete contract instance;
- canonical entrypoint name such as `execute::Transfer`;
- input/sender/environment bindings;
- a share of the parent execution-cost estimate.

If an atomic transaction contains several contract calls, integer remainder is distributed deterministically so component costs sum exactly to the measured parent cost.

Rust builds the real weighted candidate graph and schedules it with `RiskBoundedScheduler`. Component dependencies are projected back to atomic Wasmd parent transactions. Hard component dependencies are restored after projection so component-level transitive reduction cannot accidentally remove a required parent ordering edge.

Cosmos bank/funds operations are modeled as adapter-level hard resources (`bank:<logical-address>:<denom>`). They are not fabricated as CosmWasm symbolic profile relationships.

## Dependency-ready execution

The Go executor consumes `ordering_dependencies`, not scheduler waves. A transaction launches as soon as all explicit predecessors have completed; unrelated transactions in the same diagnostic level do not create a global barrier. This is the Phase 5C.6 ready-DAG semantic model documented in `docs/phase-5.md`.

When transaction `i` launches, its private transaction branch is frozen to see:

```text
immutable block base
+
successful earlier-canonical speculative versions
that had completed when i launched
```

It never gains visibility into an earlier transaction that completes after `i` has launched, and it never observes future-canonical versions.

The default Go adapter now implements the same Phase 5C.7 physical shape over Cosmos stores. One block-local multi-version index stores only successful speculative deltas keyed by canonical transaction index. A transaction captures a compact immutable visibility bit mask at launch. Point reads lazily choose the newest visible earlier-canonical version and otherwise fall through to the immutable block base; range reads merge only visible MVCC keys for the requested range. Transaction-local writes remain in the ordinary private Cosmos `CacheMultiStore` and always override MVCC reads.

A successful receipt publishes its detached final delta to the block MVCC index before dependency successors are released. Versions that complete after another transaction launches remain invisible for that transaction's lifetime. Future-canonical versions are never eligible. Nested Cosmos cache contexts preserve transaction-local write/discard semantics through a matching local overlay chain.

The previous launch-time delta materialization path remains available only as an explicit benchmark ablation with `--symbgraph-rust-visibility=materialized`; publication defaults to `mvcc`.

## Canonical correctness

Prediction is never authorization to commit.

After speculative execution, transactions reconcile in canonical block order. A speculative receipt is stale when one of its actual reads intersects an earlier canonical write which either:

1. was not visible when the receipt launched; or
2. came from an earlier transaction whose own speculative receipt had to replay.

The default validator is key-driven rather than predecessor-driven. It maintains the latest canonical writer for each exact access fingerprint plus per-store written keys for iterator/range reads. Exact reads therefore perform constant-time latest-writer checks and range reads inspect only canonical keys in the matching store. The original scan of every earlier transaction remains available as `--symbgraph-rust-validation=scan` for ablation.

A valid receipt applies only its captured transaction-local delta. A stale receipt executes again against the current canonical store and then applies its replay delta. The final Wasmd application state is compared with the matched direct-serial control in the publication harness.

## Adaptive feedback

Concrete observations are returned to the existing Rust adaptive store after reconciliation.

The bridge follows the split Phase-5 feedback sequence:

1. pre-execution pair evidence with `RuntimeFeedbackWeights::pre_execution_*`;
2. Phase-5E marginal predecessor-ready serialization delay;
3. corrected replay-execution pair evidence for pairs containing a replayed transaction;
4. targeted replay attribution with measured replay cost and invalidated-descendant fan-out;
5. whole-block service/contention economics used for default regime-change evidence decay.

The Rust planner now returns the parent transaction pairs that have an immutable symbolic profile relationship. The default Go feedback path evaluates concrete access overlap only for those profile-related pairs; this preserves candidate evidence and predicate-false candidate-miss evidence without an O(block-size²) Go fingerprint comparison. Concrete replay causes still provide targeted positive evidence, including unattributed runtime conflicts with no symbolic relationship. The previous all-pairs feedback scan is retained as `--symbgraph-rust-feedback=all-pairs` for ablation.

Candidate misses on immutable symbolic profile relationships use the existing `candidate_edge_present = false` feedback path. If a concrete conflict is purely Cosmos/Wasmd internal state and there is no symbolic profile relationship, it remains an unattributed runtime conflict rather than inventing a contract relationship.

Default serial bypass remains disabled, matching `AdaptivePlanningConfig::default()`. Default regime-change detection remains active; when service cost drops or contention rises sufficiently, stale probability/replay/serialization evidence is decayed through `AdaptiveFeedbackStore::decay_for_regime_change`.

## Files added or changed

```text
runtime/crates/acg-wasmd-scheduler-ffi/
    Cargo.toml
    include/acg_wasmd_scheduler_ffi.h
    src/lib.rs
runtime/Cargo.toml
runtime/Cargo.lock

benchmarks/cosmos-wasmd-blockstm-s3/
    main.go
    policy_runner.go
    rust_symbgraph_bridge.go
    rust_symbgraph_ffi_cgo.go
    rust_symbgraph_ffi_stub.go
    rust_symbgraph_runner.go
    rust_symbgraph_mvcc.go
    rust_symbgraph_validation.go
    rust_symbgraph_diagnostics.go
    rust_symbgraph_bridge_test.go
    rust_symbgraph_cgo_test.go
    README.rust-symbgraph-bridge.md

scripts/
    build-wasmd-symbgraph-bridge.sh
    check-vegeta-cosmos-wasmd-blockstm.sh
    run-vegeta-s3-publication-matrix.sh
    run-vegeta-s3-rust-acg-ablation.sh
    run-vegeta-s3-rust-acg-optimized-comparison.sh
tools/vegeta/
    summarize-native-s3-publication-matrix.py
    summarize-wasmd-rust-acg-ablation.py
```

## Build and validation

The authoritative validation command is:

```bash
tools/legacy-scripts/build-wasmd-symbgraph-bridge.sh
```

It runs, in order:

1. `cargo test --workspace` at the repository root, preserving every existing `crates/acg-*` unit test;
2. FFI crate tests;
3. a release Rust `staticlib` build and archive sanity check;
4. Go tests with `-tags acg_rust`, which exercise the real C ABI against checked-in S3 symbolic profiles;
5. the Wasmd harness build with the Rust scheduler linked.

The publication matrix also builds the genuine Rust static library and compiles the Wasmd harness with `-tags acg_rust`. The scheduler row emitted by that harness is now:

```text
cosmos-wasmd-symbgraph-rust
```

The CGo build tag is intentional. Ordinary Go-only tests can still compile the package through `rust_symbgraph_ffi_stub.go`, but attempting to create a Rust bridge without the tag returns an actionable error rather than silently falling back to the simplified Go scheduler.

## Integration coverage

Rust tests cover:

- loading the real checked-in CW20 symbolic profile through the real parser;
- a same-instance symbolic dependency;
- absence of a fake cross-instance global barrier;
- adapter-level bank hard dependencies;
- concrete feedback and serialization-cost updates.

Go tests cover:

- loading the checked-in symbolic document set for the FFI;
- exact multi-call parent-cost conservation;
- bank hard-resource adaptation;
- ready-DAG topology without level barriers;
- frozen launch visibility and newest-visible block-MVCC selection;
- empty-value and deletion/tombstone preservation;
- indexed exact/range canonical invalidation;
- cost-weighted DAG critical-path calculation;
- profile-scoped concrete feedback selection;
- concrete READ_WRITE / WRITE_READ / WRITE_WRITE classification;
- deterministic replay-cost splitting;
- with `-tags acg_rust`, end-to-end Go -> C ABI -> Rust ACG planning and feedback.

## Optimization ablations and instrumentation

The Rust-ACG Wasmd row emits per-block instrumentation for planning, branch creation, launch visibility, speculative contract work, delta capture/publication, validation, replay, feedback construction, Rust feedback, worker utilization, ready-queue width, dependency count, feedback-pair count, critical-path length/cost, and MVCC hit/range activity.

The default publication configuration is:

```text
visibility = mvcc
validation = indexed
feedback   = profile
```

Run the staged A/B experiment with:

```bash
VEGETA_S3_RUST_ACG_ABLATION_MODE=debug \
  bash tools/legacy-scripts/run-vegeta-s3-rust-acg-ablation.sh
```

It executes four cumulative variants:

```text
legacy        = materialized + scan    + all-pairs
mvcc          = mvcc         + scan    + all-pairs
mvcc-indexed  = mvcc         + indexed + all-pairs
optimized     = mvcc         + indexed + profile
```

and writes `summary/ablation.txt`, `summary/ablation.csv`, and `summary/ablation.json`. Once the optimized row is stable, run `tools/legacy-scripts/run-vegeta-s3-rust-acg-optimized-comparison.sh` for the full native + real-Wasmd comparison against BlockSTM, Vegeta, and the existing native AriaFB row. The normal publication matrix defaults to the optimized configuration and accepts `VEGETA_S3_RUST_ACG_VISIBILITY`, `VEGETA_S3_RUST_ACG_VALIDATION`, and `VEGETA_S3_RUST_ACG_FEEDBACK` for controlled experiments.

## Deliberate non-changes

The algorithms in root `crates/acg-*` are not replaced, copied, or deleted. The existing runtime crates are not removed. The old Go static scheduler remains available only to make historical/diagnostic comparisons possible; it is not a fallback for the Rust publication path.

## Offline profile state and online graph compaction

The Wasmd bridge keeps the same offline/online split as the root ACG implementation. Symbolic JSON
is parsed and normalized once when the scheduler state is created. The resulting immutable
`ProfileGraph`, profile lookup table, and `PreparedCandidateGraphBuilder` are retained across
blocks, including predicates compiled from the profile graph. A block therefore does not parse or
compile symbolic relationships again: it only resolves concrete transaction components to existing
`ProfileId`s, buckets them by profile/instance, and evaluates the precompiled relationships that are
actually represented in that block.

The candidate graph remains the root `acg-candidate-graph` implementation. Proven equivalence
relationships are represented as compact groups while preserving their full logical clique
semantics. This includes symmetric input-key equivalence and unconditional whole-resource
self-profile conflicts within one contract instance. Static profile adjacency and persisted
runtime-discovered fallback adjacency are both traversed, so concrete observations continue to
refine the model across blocks.

The scheduler consumes the compact candidate graph using dense edge-indexed classification. After
`RiskBoundedScheduler` performs its exact component-DAG reduction, the Wasmd adapter projects
component dependencies to atomic parent transactions, restores any required hard parent relation,
adds exact bank/funds resources, and performs one final exact parent-DAG transitive reduction. This
last reduction is necessary because projection can reintroduce edges that were redundant only after
components collapse onto atomic parent transactions.

With dependency diagnostics enabled, each block additionally reports:

- `symb_physical_candidate_edges`: relationships physically materialized by the compact graph;
- `symb_logical_candidate_edges`: full logical conflict relationships represented by those edges and
  compact groups;
- `symb_compact_candidate_groups`: number of compact equivalence groups;
- `symb_parent_dependencies_before_reduction`: projected/restored parent edges before final
  reduction;
- `symb_parent_dependencies_elided_reduction`: redundant parent edges removed before Go execution.

These are planning diagnostics only. They do not change correctness or adaptive feedback semantics.
