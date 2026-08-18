# Native S3 contract-family slots

The native S3 translation plan collapses eleven high-impact Ethereum source/profile families into
seven reusable CosmWasm code-family slots while retaining every Ethereum storage-owner address as a
separate native instance namespace.

Expected source roots:

- `cw20-base/`
- `controlled-cw20/`
- `fee-token-cw20/`
- `astroport-pair/`
- `wrapped-native-token/`
- `cw721-mintable/`
- `xen-like/`

This patch only freezes the mapping and builds the pre-execution translation plan. The contract
implementations are intentionally not fabricated here. Their source and the corresponding genuine
LLM symbolic analyses are the execution gate for the subsequent benchmark stage.
