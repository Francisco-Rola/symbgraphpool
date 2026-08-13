# Brick 3 — Runtime feedback and adaptive profile-edge statistics

Brick 3 closes the loop between the symbolic conflict graph and concrete execution. It does not
change consensus or scheduling correctness. The immutable profile graph remains topology; adaptive
statistics and runtime-discovered fallback edges live in a separate mutable feedback store.

## Modules

### `crates/acg-feedback`

Runtime-neutral feedback state:

- positive/negative `ConflictObservation` records;
- explicit observation sources (`pre_execution`, `canonical_execution`, `validation`, `replay`);
- decayed Beta-Bernoulli statistics;
- confidence derived from posterior mass;
- per-block `ObservationBuffer` batching;
- runtime-discovered fallback profile edges;
- stable-key-based JSON checkpoints and restore.

### `runtime/crates/acg-runtime-feedback`

CosmWasm/validator-specific attribution:

- exact storage and bank read/write conflict detection;
- storage-range scan versus write detection;
- contract-local storage scope and globally keyed bank scope;
- optional reverted-access inclusion for audit analysis;
- candidate-edge negative evidence only after successful concrete comparison;
- static predicate-miss attribution;
- runtime topology-miss discovery;
- future-facing pair-specific validation/invalidation/replay ingestion;
- `RuntimeFeedbackEngine` facade for collect -> batch -> update -> checkpoint.

## Evidence semantics

An observation is not created merely because two transactions appeared in one block.

Positive evidence is emitted when concrete accesses overlap as read/write, write/read, or
write/write, or when a future validator explicitly attributes invalidation/replay to a predecessor.

Negative evidence is emitted only when a pair was actually tracked for comparison:

1. it had a materialized candidate edge and both transactions executed successfully, or
2. the profile pair already has a runtime-discovered fallback edge and both transactions executed
   successfully, or
3. a future validation component explicitly reports pair-specific independence.

A pair that was never compared contributes no evidence. Failed top-level transactions currently
also contribute no negative evidence because the existing engine returns no top-level failure
access artifact.

## Concrete access attribution

Exact access indexes distinguish two scopes:

```text
contract storage: (contract address, key bytes)
bank state:       (address+denom encoded key bytes)
```

Contract storage with identical key bytes in two different contract instances is disjoint. Bank
keys are global host-state keys, so their invoking contract does not create an independent
namespace.

`StorageScan` is a read-range access. It conflicts with storage writes/removals in the same
contract whose exact key falls inside `[start, end)`. Scan/scan is read/read and does not conflict.

By default, reverted nested accesses are excluded from canonical conflict updates. They remain in
the engine trace and can be included with `TraceConflictConfig { include_reverted_accesses: true }`
for audit/analysis experiments.

## Decayed Beta updates

Each static or fallback edge stores:

```text
alpha
beta
last_update_epoch
positive_observations
negative_observations
```

At a touched epoch `t`, old evidence is retained by:

```text
alpha <- lambda^delta * alpha
beta  <- lambda^delta * beta
```

Then positive evidence of weight `w` increments `alpha`; negative evidence increments `beta`.
The posterior mean is:

```text
p = alpha / (alpha + beta)
```

Confidence is operationally derived from posterior mass:

```text
confidence = 1 - exp(-(alpha + beta) / confidence_scale)
```

The default retention factor is `0.99`. Default source weights are:

```text
pre-execution conflict/independence: 1.0
canonical conflict/independence:     3.0
validation conflict/independence:    3.0
replay conflict:                     4.0
```

Thus canonical/validation evidence dominates speculative pre-execution evidence by default.

## Runtime-discovered fallback edges

If concrete execution finds a conflict between profiles with no static symbolic edge, the collector
emits a `RuntimeDiscovered` observation. Applying that observation creates a fallback edge with:

- stable profile endpoints;
- a conservative configurable prior;
- accumulated concrete conflict kinds;
- discovery epoch;
- `review_required = true`;
- its own decayed Beta statistics.

Fallback edges collect future negative evidence for the same profile pair and are persisted in the
feedback checkpoint. They intentionally remain separate from the immutable static profile graph.
Brick 4 will make these fallback relationships direct inputs to weighted candidate-edge
materialization and scheduling.

## Static predicate misses versus topology misses

A concrete overlap can be absent from the candidate graph for two reasons:

1. a static profile edge exists, but its concrete predicate incorrectly evaluated/pruned the pair;
2. no static profile edge exists at all.

Case 1 updates the existing static profile edge. Case 2 creates/updates a fallback edge. Both are
counted as candidate misses, while `fallback_edges_created` separately identifies topology misses.

## Batching

The runtime produces an `ObservationBuffer` first. `AdaptiveFeedbackStore::apply_batch` then sorts
observations by epoch and applies decay/update once through the mutable statistics store. Shared
statistics are therefore not mutated for every individual VM storage access.

## Checkpointing

`FeedbackCheckpoint` uses stable profile keys rather than dense local `ProfileId`s. It contains all
static edge statistics and all runtime-discovered fallback edges. Dense IDs are resolved again on
restore.

The checkpoint format is independently versioned. Brick 5E currently writes version `3`; restore remains backward-compatible with v1 (conflict statistics only) and v2 (conflict + replay-cost statistics).

## Future validation/replay integration

`ValidationEvidence` already supports:

```text
Independent
Invalidated { conflict_kinds }
Replayed    { conflict_kinds }
```

Brick 5's speculative executor can emit these directly after canonical-order validation without
changing the feedback/statistics layer.

## Validation commands

Root feedback/statistics tests:

```bash
cargo test -p acg-feedback --test statistics -- --nocapture
```

Runtime attribution tests:

```bash
cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-runtime-feedback \
  --test runtime_feedback \
  -- \
  --nocapture
```

Full root gates:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Full runtime gates:

```bash
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

## Brick 3 boundary

Brick 3 learns probabilities but does not yet attach those probabilities to concrete candidate
edges or schedule from them. The existing candidate graph remains symbolic `True/Unknown` topology.
Brick 4 consumes this store to produce weighted hard/soft transaction edges and a risk-bounded wave
scheduler.
