# Brick 2: concrete transaction graph

Brick 2 connects the persistent symbolic profile graph to concrete transactions produced by the
single-validator runtime. It does not execute transactions or schedule waves. Its output is a
runtime-independent `CandidateGraph` that later schedulers can consume.

## Data flow

```text
ProducedBlock / ExecutionRequest
        |
        v
CosmWasmCandidateAdapter
        |
        +-- contract code checksum -> StableProfileKey -> ProfileId
        +-- contract address       -> InstanceId
        +-- execute JSON           -> canonical entrypoint selector
        +-- sender/funds/message   -> InputBindings
        |
        v
Vec<CandidateTransaction>
        |
        v
CandidateGraphBuilder
        |
        +-- bucket by ProfileId
        +-- traverse persistent profile edges only
        +-- evaluate precompiled predicates
        +-- false   -> prune pair
        +-- true    -> materialize edge
        +-- unknown -> materialize conservative edge
        |
        v
CandidateGraph
```

## Crates

### `acg-predicate`

Compiles `PredicateTemplate` analyzer expressions once into an executable AST. The evaluator uses
three-valued logic:

- `true`: the concrete pair satisfies a symbolic conflict clause;
- `false`: the pair can be pruned safely for the represented clause;
- `unknown`: state-derived, wildcard, missing, or unsupported information prevents a proof of
  independence.

Supported key expressions include scalar paths, tuple keys, fixed array indexes and synchronized
symbolic indexes such as:

```text
account
(owner, spender)
(warehouse_id, district_id, customer_id)
(lines[i].supply_warehouse_id, lines[i].item_id)
```

Input-only guard comparisons and boolean conjunction/disjunction are evaluated when possible.
Supported guard operands also honor analyzer delegation input mappings. State-dependent guards
remain `unknown` unless their input portion already proves them false.

### `acg-candidate-graph`

Defines `CandidateTransaction`, `TransactionEdge`, `CandidateGraph`, and
`CandidateGraphBuilder`. Dense transaction indexes are block-local. The builder does not perform
an all-pairs scan across the block: transactions are bucketed by profile and only profile pairs
present in the persistent adjacency graph are considered. When every alternative clause requires
the same contract instance, the builder additionally buckets by `(ProfileId, InstanceId)` and never
evaluates impossible cross-instance pairs.

For a same-profile edge, the builder considers each concrete pair once. For cross-profile edges,
it evaluates the cartesian product only between the two relevant profile buckets. Later Brick 5
indexing can reduce large bucket products further using instantiated resource fingerprints.

### `acg-cosmwasm-adapter`

Lives in the `runtime/` workspace and converts existing runtime requests into candidate
transactions. `ExecutionRequest` remains the execution source of truth.

The default execute decoder supports ordinary externally tagged CosmWasm JSON enums:

```json
{"transfer":{"from":"alice","to":"bob","amount":"10"}}
```

which resolves to `execute::Transfer`. A pluggable `ExecuteEntrypointDecoder` exists for contracts
with nested dispatch structures such as hook messages or nested update enums.

`InstanceId` is intentionally validator-local. Two addresses with the same code checksum get the
same profile but distinct instance IDs. Pending top-level instantiations use the engine's deterministic predicted contract address, so
the pre-execution `InstanceId` remains the same after that address is committed.

## ConflictLab acceptance cases

Brick 2 tests the following concrete behavior:

```text
Credit(alice) x Credit(alice)        -> true edge
Credit(alice) x Credit(bob)          -> no edge
IncrementCounter(1) x Increment(1)   -> true edge
IncrementCounter(1) x Increment(2)   -> no edge
same key, different contract address -> no contract-storage edge
ResetAllBalances x Credit(alice)     -> unknown/conservative edge
invalid Transfer sender              -> input guard can prune edge
```

The runtime integration test additionally constructs real `ExecutionRequest` values, resolves the
actual engine code checksum and contract metadata, adapts a produced block, and builds the
candidate graph.

## Validate

Root graph workspace:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

Predicate tests only:

```bash
cargo test -p acg-predicate --test predicate -- --nocapture
```

Candidate graph tests only:

```bash
cargo test -p acg-candidate-graph --test candidate_graph -- --nocapture
```

Runtime adapter:

```bash
cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
```

ConflictLab runtime-to-graph integration only:

```bash
cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-cosmwasm-adapter \
  --test conflictlab \
  -- \
  --nocapture
```

## Deferred from Brick 2

- state lookups during symbolic guard evaluation;
- resource-fingerprint indexes inside large profile buckets;
- transaction-edge probabilities and adaptive history;
- block scheduling from the candidate graph;
- speculative parallel execution, validation, and replay;
- generic decoding of arbitrary nested contract-specific execute enums.

Those boundaries are explicit so unsupported information remains conservative rather than being
silently interpreted as independence.
