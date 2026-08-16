# Audit of the uploaded CosmosSE CosmWasm fork

## Source and scope

The audit used the uploaded `cosmosSE-master.zip`, whose archive comment identifies commit
`b4c775cc0146ffda35361485e9897de0d63fff85`. The user indicated that relevant experimental changes
were concentrated in `packages/vm`; dependencies in `packages/std` and `packages/crypto` were also
inspected where the VM depended on fork-specific behavior.

The goal was not to certify consensus safety. It was to identify which parts should be retained for
an execution-only research engine and which experimental code should not be migrated.

## Main findings

### 1. Speculation was inserted into the VM backend interface

The fork added transaction lifecycle methods such as `checkpoint`, `commit`, `wipe_tmp`,
`get_validation`, `set_validation`, `update_env`, and `get_tx_result` directly to the public
`Storage` and `Querier` traits. This makes every VM backend responsible for one particular
speculative-execution protocol and couples ordinary contract execution to Block-STM experiments.

**Resolution:** the new engine uses the normal CosmWasm VM traits. Transaction overlays, rollback,
access traces and future speculative versions live in host-side engine modules.

### 2. The mock bank implementation was not a global ledger

`MockQuerier` stored one `Vec<Coin>` cache per contract backend, initialized several balances from
hard-coded Neutron addresses, and frequently ignored the queried address when selecting the local
balance vectors. This cannot correctly represent transfers between arbitrary accounts or contracts.

**Resolution:** the new engine has one transactionally overlaid ledger keyed by `(address, denom)`.
Transfers check funds, preserve value, support multiple denominations, and commit atomically with
contract storage.

### 3. Nested queries took ownership out of a shared backend map

The fork removed a backend from a `DashMap` to execute a remote query and later inserted it back.
Errors, panics or recursive access can lose the backend or create re-entrancy hazards. The query path
also contains `unwrap`/`expect` calls on untrusted query bytes and VM results.

**Resolution:** storage and bank state are shared through a transaction overlay. Every VM invocation
gets a short-lived backend view; nested smart queries recursively create another view without moving
contract ownership out of a global map. Errors are propagated as typed engine errors.

### 4. Block-STM support was incomplete

`STMStorage` and parts of `STMQuerier` contain `todo!()` implementations for core storage methods.
Those types are exported from the VM testing module despite not being operational.

**Resolution:** no incomplete MVCC implementation was migrated. A later runtime phase can implement
versioned storage behind the engine's transaction-state boundary and test it independently.

### 5. Chain-specific behavior leaked into general CosmWasm types

The fork changed the default `CosmosMsg` custom type to a Neutron-specific message and added
chain-specific dependencies and binaries to `cosmwasm-vm`. This makes the VM less reusable and
complicates contract compatibility.

**Resolution:** the engine uses `CosmosMsg<Empty>` and explicitly rejects unsupported custom,
staking, governance, IBC and Stargate messages. Chain extensions can later be registered through a
separate host-module interface.

### 6. Experimental binaries dominated the package

Hundreds of trace Wasm files and large prototype binaries were stored below `packages/vm/src/bin`,
making the VM package roughly 299 MB while the reusable VM source was only a small fraction of that.

**Resolution:** none of the trace corpus or prototype binaries is vendored. One upstream Hackatom
Wasm fixture is retained solely for a real-VM smoke test.

### 7. Transaction and submessage rollback semantics were fragmented

Storage and bank state had independent temporary maps and checkpoints, while nested calls assembled
read sets in the querier. There was no single host transaction object enforcing atomic state across
storage, funds, contract creation and replies.

**Resolution:** the new engine uses one shared transaction overlay. Top-level failure discards the
entire overlay. A failed submessage restores its checkpoint; handled failures can continue through
`reply`, while attempted accesses remain in the trace and are marked `reverted`.

## Retained ideas

The following concepts were useful and were reimplemented cleanly:

- transaction-local writes layered over committed state;
- explicit storage and bank read/write traces;
- contract-address-aware storage keys;
- nested contract execution and smart queries;
- delayed commit and rollback;
- a future boundary for validation and conflict feedback.

## Current limitations

The first extracted engine is intentionally correctness-oriented:

- Wasm modules are compiled for each call rather than cached;
- committed state is in memory;
- gas used by nested messages is not yet accumulated into reply metadata;
- nested `SubMsg.gas_limit` values are not independently metered yet;
- nested reply `gas_used` is currently reported as zero;
- migration/admin operations and custom chain messages are rejected;
- contract addresses use transaction-local deterministic derivation rather than a chain-specific address algorithm;
- there is no parallel execution or MVCC yet;
- `EngineApi` uses deterministic UTF-8 address canonicalization rather than a chain-specific codec.

The engine uses published CosmWasm 2.0.9 rather than the uploaded 2.0.0 release candidate. The
2.0.9 choice is intentional: versions before 2.0.6 are affected by the 2024 VM gas-mispricing
advisory. These remaining limitations are explicit extension points and do not require modifying
`cosmwasm-vm`.
