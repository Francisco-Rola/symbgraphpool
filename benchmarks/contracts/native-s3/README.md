# Vegeta native CosmWasm contract-family registry

This directory contains the concrete CosmWasm implementations used by the Vegeta native ports.
The original S3 workload uses ten reviewed families; S1 adds three source-reviewed contract artifacts for
its expanded semantics: `cw721-drop`, `stargate-cw20`, and `fiat-token-cw20`. S1 also splits the
Universal Router into a dedicated semantic family while reusing the existing source-derived router-lock Wasm. Ethereum
storage-owner addresses still instantiate independent native state namespaces even when they share
one code family.

Implemented families:

- `cw20-base`: balances, allowances, transfer/transfer-from, burn/mint and standard queries.
- `controlled-cw20`: CW20-like state plus pause, blacklist and controlled mint/burn state.
- `fiat-token-cw20`: S1-only FiatToken/USDC analogue adding burn and permit allowance+nonce state without changing the frozen S3 controlled-token binary.
- `fee-token-cw20`: fee-on-transfer policy with a configured collector.
- `wrapped-native-token`: native-denom deposit/withdraw plus fungible-token state.
- `cw721-mintable`: NFT owner, per-token approval, operator approval, owner counts and minting.
- `astroport-pair`: compact constant-product reserves plus LP balances.
- `xen-like`: global rank, user mint records, staking records and fungible balances.
- `cw1155-like`: multi-token balances and operator approvals.
- `marketplace-router`: order-status and per-address counter state for marketplace selectors.
- `operator-filter-helper`: subscription and operator-policy state.
- `cw721-drop`: reviewed S1 NFT mint/drop semantics including ownership, approvals, supply, per-wallet/stage counters, and nonce state.
- `stargate-cw20`: reviewed S1 STG analogue with balances/allowances plus main-endpoint bridge escrow send/receive state; bridge operations do not change total supply.

These are workload analogues for the state semantics actually selected by the reviewed S3/S1 translations. They
are not byte-for-byte ports of the Ethereum contracts and do not claim identical gas schedules.
The source is the ground truth for the symbolic analyses in `benchmarks/symbolic/native-s3/`.
Concrete historical Ethereum read/write keys are not inputs to any of these contracts.

### Bundle execution

The selected contracts are executed by `runtime/crates/acg-vegeta-native-s3-executor` through the
CosmWasm engine's atomic `BundleCall` API. A bundle corresponds to one Ethereum transaction and may
contain calls to multiple native contract instances plus smart queries and bank sends. Contract
instances remain namespaced by the original Ethereum storage-owner address; code is shared by
native code family. Setup/priming runs before the 101 measured S3 blocks and is excluded from the
concrete access trace used for topology fidelity.
