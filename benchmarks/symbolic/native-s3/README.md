# Native S3 symbolic-analysis slots

This directory is reserved for the **genuine LLM-generated symbolic analyses** consumed by
SymbGraphPool for the seven native code families frozen in
`evaluation/vegeta/s3-native-family-map.v1.json`.

The native S3 planner intentionally references the following paths before they exist:

- `cw20-base.symbolic.json`
- `controlled-cw20.symbolic.json`
- `fee-token-cw20.symbolic.json`
- `astroport-pair.symbolic.json`
- `wrapped-native-token.symbolic.json`
- `cw721-mintable.symbolic.json`
- `xen-like.symbolic.json`

Do **not** create synthetic placeholder profiles just to make the execution gate pass. Each file
must be generated from the real native CosmWasm source in the same production format as the existing
LLM symbolic-analysis artifacts. Translation/preflight evaluation is allowed while these slots are
missing; production SymbGraph evaluation is not.
