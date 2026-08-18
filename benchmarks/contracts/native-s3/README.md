# Vegeta S3 native CosmWasm contract families

This directory contains the concrete CosmWasm implementations used by the native Vegeta S3 port.
The eleven conflict-dominant Ethereum source families reuse seven base native families, while the
selector-granular background pass adds three compact semantic extension families. Ethereum
storage-owner addresses still instantiate independent native state namespaces even when they share
one code family.

Implemented families:

- `cw20-base`: balances, allowances, transfer/transfer-from, burn/mint and standard queries.
- `controlled-cw20`: CW20-like state plus pause, blacklist and controlled mint/burn state.
- `fee-token-cw20`: fee-on-transfer policy with a configured collector.
- `wrapped-native-token`: native-denom deposit/withdraw plus fungible-token state.
- `cw721-mintable`: NFT owner, per-token approval, operator approval, owner counts and minting.
- `astroport-pair`: compact constant-product reserves plus LP balances.
- `xen-like`: global rank, user mint records, staking records and fungible balances.
- `cw1155-like`: multi-token balances and operator approvals.
- `marketplace-router`: order-status and per-address counter state for marketplace selectors.
- `operator-filter-helper`: subscription and operator-policy state.

These are workload analogues for the state semantics actually selected by the S3 translation. They
are not byte-for-byte ports of the Ethereum contracts and do not claim identical gas schedules.
The source is the ground truth for the symbolic analyses in `benchmarks/symbolic/native-s3/`.
Concrete historical Ethereum read/write keys are not inputs to any of these contracts.

### Bundle execution

The ten contracts are executed by `runtime/crates/acg-vegeta-native-s3-executor` through the
CosmWasm engine's atomic `BundleCall` API. A bundle corresponds to one Ethereum transaction and may
contain calls to multiple native contract instances plus smart queries and bank sends. Contract
instances remain namespaced by the original Ethereum storage-owner address; code is shared by
native code family. Setup/priming runs before the 101 measured S3 blocks and is excluded from the
concrete access trace used for topology fidelity.
