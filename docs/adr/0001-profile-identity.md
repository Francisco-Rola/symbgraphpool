# ADR 0001: Stable profile keys and dense local IDs

- Status: accepted for phase 1
- Date: 2026-08-06

## Context

A profile needs a persistence identity that survives process restarts and artifact exchange, while
online graph traversal needs compact array indexes. Persisting a process-local integer would make
artifacts dependent on load order. Using a 32-byte hash in every adjacency entry would increase
memory traffic in the validator hot path.

The analyzer fixture supplies an entrypoint name but does not supply the runtime identifier,
contract code hash, or a chain-native numeric selector.

## Decision

Use two identities:

1. `StableProfileKey([u8; 32])` for artifacts, persistence, and cross-process references.
2. `ProfileId(u32)` as a dense local index assigned when an artifact is loaded.

The stable key is BLAKE3 over this canonical byte sequence:

```text
"acg.profile-key.v1\0"
u32_be(runtime_id_utf8_length)
runtime_id_utf8
contract_code_hash[32]
u16_be(entrypoint_kind_tag)
u64_be(numeric_entrypoint_selector)
u16_be(profile_schema_version)
```

Runtime IDs are trimmed, ASCII-lowercased, and restricted to `[a-z0-9._-]`.

Entrypoint kind tags are:

```text
1 instantiate
2 execute
3 query
4 reply
5 migrate
65535 other
```

When a runtime-native selector is unavailable, the fallback selector is the first eight bytes,
interpreted as big-endian `u64`, of:

```text
BLAKE3("acg.entrypoint-selector.v1\0" || u32_be(name_length) || entrypoint_name_utf8)
```

The ingestion layer detects selector collisions within one analyzer document. A runtime adapter may
provide explicit selector overrides.

At graph load, profiles are sorted lexicographically by `StableProfileKey` and assigned IDs in
`0..N`. Dense IDs are never serialized as stable identity.

## Consequences

- Artifact ordering does not affect local IDs or adjacency layout.
- Contract upgrades produce new identities through `contract_code_hash`.
- Profile schema changes can intentionally invalidate identities through `profile_schema_version`.
- Different contract instances sharing the same code also share a profile; instance identity stays
  separate and is evaluated by edge predicates.
- Cross-language implementations must follow the canonical encoding exactly.
