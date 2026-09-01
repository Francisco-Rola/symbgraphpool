# Vegeta native source-derived symbolic analyses

The JSON artifacts in this directory are the production-format symbolic analyses for the reviewed
native CosmWasm sources under `benchmarks/contracts/native-s3/`. The original S3 registry has ten
families; S1 adds `cw721-drop`, `stargate-cw20`, and the S1-only `fiat-token-cw20` artifact.
Universal Router uses the existing `marketplace-router` source-derived router-lock profile under a
dedicated S1 semantic family so the frozen S3 artifact/binary remains unchanged.

Each artifact uses schema `3.1-compact-config-field-groups` and includes an
`analysis_provenance` record with:

- `method = llm-source-derived`;
- the SHA-256 of the complete checked-in source file;
- an explicit statement that historical Ethereum trace keys were not analyzer inputs.

Every access also contains line-scoped source evidence. `validate-native-s3-implementation.py`
fails if a source hash changes, an evidence snippet no longer matches its declared lines, a required
profile disappears, or a native source contains the old trace-oracle array names. This makes source
changes require an intentional symbolic-analysis refresh rather than silently using stale profiles.

The seven conflict-dominant families are frozen by
`evaluation/vegeta/s3-native-family-map.v1.json`. `cw1155-like`, `marketplace-router`, and
`operator-filter-helper` are selector-granular semantic extensions from the final background pass.

For the S1 extensions, selector/owner inclusion is reviewed separately by `s1-native-family-map.v2.json`; unknown selectors on a mapped owner remain opaque and do not count toward the selector-aware semantic conflict gate.
