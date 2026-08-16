# Adaptive Conflict Graph architecture through Phase 2

## Trust boundary

`acg-symbolic-json` treats analyzer output as untrusted input. Parsing, normalization, profile-edge
derivation, and predicate compilation happen outside the latency-sensitive scheduling loop. The
portable `ProfileGraphArtifact` remains the persistence handoff boundary.

## Offline profile flow

```text
symbolic analyzer JSON
        |
        v
RawAnalyzerDocument                 acg-symbolic-json::raw
        |
        v
normalized ProfileDefinition[]      acg-symbolic-json::normalize
(delegations recursively composed)
        |
        +--> resource/access index
        |          |
        |          v
        |    ProfileEdgeDefinition[] acg-profile-graph::derive
        |
        v
ProfileGraphArtifact                portable, stable-key based
        |
        v
ProfileGraph::load
        |
        +--> stable key -> dense ProfileId registry
        +--> edge endpoint resolution
        +--> CSR adjacency arrays
```

## Online candidate flow

```text
validator ProducedBlock
        |
        v
acg-cosmwasm-adapter
        |
        +--> code checksum + numeric entrypoint selector -> ProfileId
        +--> contract address / pending instantiate       -> InstanceId
        +--> JSON payload + info + env                    -> InputBindings
        |
        v
CandidateTransaction[]
        |
        v
CandidateGraphBuilder
        |
        +--> profile buckets
        +--> persistent profile adjacencies only
        +--> CompiledPredicate evaluation
        |
        v
CandidateGraph
        |
        +--> true edges
        +--> conservative unknown edges
        +--> false pairs pruned
```

## Stable versus local identity

- `StableProfileKey`: serialized, content-derived, independent of load order.
- `ProfileId`: dense validator-local profile index used in persistent graph arrays.
- `InstanceId`: dense validator-local contract-instance index. Multiple deployed contracts can
  share one profile while keeping disjoint contract-local storage.
- `TxIndex`: dense block-local transaction index used by candidate topology.
- `TxId`: logical runtime transaction identifier retained for tracing; topology does not depend on
  it being globally unique.
- `entrypoint_name`: retained for diagnostics and offline selector derivation, not graph traversal.

## Predicate semantics

`acg-predicate` converts declarative profile-edge clauses into executable expression trees once.
Evaluation is three-valued:

```text
False    concrete bindings prove this clause cannot overlap
True     concrete bindings satisfy at least one conflict clause
Unknown  state-derived, wildcard, missing, or unsupported information remains
```

`Unknown` is never treated as independence. Candidate graph construction materializes it so later
weighted scheduling can assign a conservative probability.

Input expressions currently support scalar paths, tuple keys, fixed indexes, and one synchronized
symbolic array variable. This covers ConflictLab and prepares MiniWarehouse expressions such as
`(lines[i].supply_warehouse_id, lines[i].item_id)`.

## Determinism

Persistent graph loading sorts profiles by stable key and edges by resolved endpoint pair.
Candidate transactions retain block order, and candidate edges are sorted by dense transaction
indexes and profile-edge index before CSR adjacency construction.

`InstanceId` values are local registry handles; their numeric values are not persisted or compared
across validators. Only equality matters for contract-local predicate evaluation.

## Current limitations

- The analyzer schema currently focuses on contract-local storage resources; native bank effects
  and arbitrary external module effects are not yet profile resources.
- State-dependent guards are not queried from live state during candidate construction. They remain
  `Unknown` unless an input-only prefix already proves them false.
- The default CosmWasm execute decoder handles top-level externally tagged enums. Nested hook or
  contract-specific dispatch uses the pluggable decoder interface.
- Profile buckets can still require a cartesian product when many transactions share a possible
  profile edge. Resource-key fingerprint indexes are a later performance-hardening step.
- Candidate edges are binary/trivalent at this stage; adaptive probabilities are introduced after
  runtime observation support.
