# Vegeta S3 source-derived symbolic analyses

The ten JSON artifacts in this directory are the production-format symbolic analyses for the real
native S3 CosmWasm sources under `benchmarks/contracts/native-s3/`.

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
