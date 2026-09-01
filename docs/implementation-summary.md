# Adaptive Conflict Graph — implementation summary

## System objective

Adaptive Conflict Graph (ACG) moves as much smart-contract execution work as possible before the
consensus decision while preserving deterministic canonical-order state-machine semantics. Static
symbolic information predicts conflicts; runtime observations refine those predictions; a
risk-bounded scheduler exposes safe speculative parallelism; concrete canonical validation and
selective replay remain the correctness authority.

## Offline symbolic pipeline

Contract analyzers emit symbolic entrypoint profiles. The Rust normalization layer converts them to
stable profile identities and conflict clauses without persisting dense in-memory IDs. A persistent
`ProfileGraph` stores profile relationships. Predicates are compiled once when this graph is loaded,
not on every block. Wasmd uses the same checked-in profile corpus through the Rust FFI; Go does not
reimplement the publication scheduler.

## Online transaction graph

For each block, concrete transaction inputs instantiate the persistent profile relationships. A
transaction may contain multiple contract calls, but the scheduler graph uses the atomic Cosmos
transaction as its node. Entry-point/profile relationships remain evidence attached to transaction
pairs and therefore remain available for adaptive feedback.

The candidate graph uses several forms of compaction:

- exact profile/instance bucketing avoids irrelevant pair comparisons;
- equivalence-key groups encode proven conflict cliques as a compact chain plus logical-group
  metadata;
- repeated profile evidence for the same atomic pair is grouped without inventing a synthetic
  probability model;
- dense edge indexes and arrays avoid ordered-map bookkeeping in the scheduler;
- the scheduler performs exact transitive reduction;
- adapter-level bank/funds dependencies are merged and the final atomic DAG is reduced again.

## Conflict predicates and semantic conservatism

Predicates distinguish whole-resource conflicts from input-dependent equivalence relationships.
When the symbolic model is incomplete, the runtime can conservatively retain candidate edges or
learn runtime-discovered fallback relationships. Unsupported/missing Wasmd symbolic profiles do not
receive fabricated aliases: those calls execute normally and concrete validation preserves
correctness.

## Runtime feedback and learning

Concrete execution produces positive and negative conflict evidence. The feedback model tracks
probability/confidence, maturity, conflict misses, replay cost, invalidation/fan-out effects and
serialization cost. Stable relationship identities allow observations to survive graph reloads and
feed later blocks. Static profile relationships and runtime-discovered relationships share the same
adaptive scheduling path.

## Risk-bounded scheduling

`RiskBoundedScheduler` classifies candidate evidence as Low, Soft or Hard. Hard relationships order
transactions. Soft relationships may be serialized according to aggregate risk, confidence,
maturity, cost and configured risk budget. Parallel soft evidence between the same transaction pair
uses the scheduler's existing `1 - product(1-p)` semantics; any hard constituent dominates. The
resulting dependency graph is an exact reduced DAG.

## CosmWasm/Wasmd execution path

The Rust crate `acg-wasmd-scheduler-ffi` is a static library called by the Go Wasmd benchmark. Rust
owns symbolic parsing, candidate graph construction, conflict predicates, adaptive state and
scheduling. Go owns the actual Cosmos SDK/Wasmd/WasmVM execution substrate.

The executor launches dependency-ready transactions over block-local MVCC visibility. Each
transaction executes in an isolated cache/overlay, records exact reads, range reads and writes, and
produces a detached receipt/delta. Post-consensus reconciliation walks canonical transaction order,
reuses receipts whose read view is still valid, and replays only stale transactions. Key-indexed
validation avoids scanning every earlier transaction. Empty byte values are preserved distinctly
from deletions when captured deltas are applied.

## Correctness boundary

All optimized schedulers are checked against direct canonical serial execution from an independently
initialized but byte-identical application state. After every block the complete application state
digest must match. Symbolic prediction is therefore a performance mechanism, not a correctness
assumption.

## Important implementation optimizations

The current path incorporates the main optimization work from the implementation evolution:

1. persistent prepared symbolic predicates/profile lookup rather than per-block recompilation;
2. atomic transaction scheduling rather than component-call scheduling followed by projection;
3. dense candidate/scheduler structures and profile-topology-driven feedback pair construction;
4. block-local MVCC launch visibility instead of materializing predecessor deltas repeatedly;
5. key-indexed canonical validation;
6. profile-scoped adaptive feedback rather than all-pairs updates;
7. exact final parent-DAG transitive reduction after Cosmos bank/funds dependencies;
8. detailed phase, critical-path, dependency-provenance, oracle-DAG and replay diagnostics.

## Evaluation baselines on the same Wasmd machinery

The maintained controlled evaluator now runs five strategies over the same state machine:

- direct serial;
- Cosmos SDK BlockSTM;
- AriaFB same-Wasmd port with exact Rule-2 aborts and completion-driven hot-chain dependency-DAG fallback;
- Vegeta same-Wasmd `SpeculateMod`/`ParallelMod` port with hot-key proposal reordering, Rule-2 replay batches, and access-change handling;
- Rust-ACG pre-consensus symbolic/adaptive scheduling + canonical validation/replay.

AriaFB and Vegeta are mechanism ports over the same Wasmd execution substrate, not source-code identity
with the upstream Ethereum engine. Unlike the earlier conservative baselines, they no longer force
historical canonical-order semantics: Vegeta uses its proposal reorder and AriaFB uses a Rule-2-valid
serialization. Each row is checked against an independent serial execution of that derived order; its
`matched_serial_nanos` uses the same order, while `historical_serial_nanos` retains the common original-order
timing control.

## Evaluation metrics

The publication harness derives one fixed campaign consensus window `C` as the longest measured
pre-consensus interval among ACG and Vegeta. Its primary execution-limited throughput metric charges the same
`C + post-consensus` block-cycle denominator to every system. Rust-ACG and Vegeta use `C` for
pre-execution; serial, BlockSTM and AriaFB wait through the same consensus interval before their
post-consensus execution. The evaluator additionally reports post-x (serial/post-consensus),
actual wall-x, replay rate, validation and replay-execution time, pre-consensus tail latency and
confidence intervals.

## Current evaluation interpretation

Preliminary S3 runs show that the Rust graph-planning path has been reduced to sub-millisecond or
low-millisecond work on representative blocks, while post-consensus replay can dominate individual
blocks. This shifts the next optimization emphasis from generic graph representation toward
post-consensus-aware adaptive feedback, replay attribution, and policy choices that treat
pre-consensus serialization as cheap when it fits inside the consensus budget.

## Publication roadmap

The next research steps are: stabilize the five-way Wasmd comparison; run physical-core scaling on a
dedicated host; add workload families; freeze externally defined consensus-window sensitivity;
measure adaptation/ablation; integrate with a real consensus window; and package a reproducible
artifact. `evaluation/wasmd/README.md` contains the detailed OSDI/EuroSys-oriented plan.
