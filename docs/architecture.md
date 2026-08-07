# Brick 1 architecture

## Trust boundary

`acg-symbolic-json` treats analyzer output as untrusted input. Parsing and normalization happen
outside the validator scheduling path. The emitted graph artifact is the handoff boundary.

## Data flow

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

## Stable versus local identity

- `StableProfileKey`: serialized, content-derived, independent of load order.
- `ProfileId`: dense local index used in arrays and hot-path graph traversal.
- `entrypoint_name`: retained for diagnostics and selector-collision checks, not used in the hot
  path.

## Determinism

Graph loading sorts profiles by stable key and edges by resolved endpoint pair. Artifact producers
may serialize profiles in any order without changing local IDs or adjacency layout.

## Edge derivation limitations in brick 1

- The analyzer example only describes contract-local storage resources.
- Calls, balances, environment effects, wildcard effects, and cross-contract resources are not yet
  represented by its JSON schema.
- Guards are preserved as source expressions plus delegation binding frames but are not evaluated.
- Dynamic resource keys with missing input origins become `Unresolved` clauses and therefore
  `Unknown` profile relations.
