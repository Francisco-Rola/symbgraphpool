# Vegeta workload tooling

This directory owns source-trace collection, frozen-family characterization, native-plan construction, validation, and summarization helpers for Vegeta-derived workloads. `evaluation/` contains the paper-facing experiment entrypoints; translation internals stay here.

## S4 after source collection

Once `run-vegeta-s4-native-inputs.sh` reports that S4 native-characterization inputs are frozen, run:

```bash
bash tools/vegeta/run-vegeta-s4-characterize.sh
```

This stage is local-only. It validates the 5,000-block range, writes `corpus-provenance.{json,txt}`, records (but does not normalize away) any difference from Vegeta's published S4 transaction count, ranks runtime families/selectors, bootstraps only identical bytecode mappings already reviewed for S1/S3, and computes exact source conflict coverage plus conflict-relevant/all-storage/state-owner and conservative all-or-nothing transaction-gas diagnostics. It also writes `family-blocker-clusters.json` and `family-marginal-coverage.{json,txt}`. The planner emits balanced, access-first, and conflict-first planning views with exact conflict-union recomputation; only the exact source-conflict and median conflict-bearing-block metrics gate family freeze. Access and strict-gas views remain transparent diagnostics/planning references and never become executable mappings automatically.

It also creates two persistent human-editable review inputs:

```text
benchmarks/corpora/vegeta-ethereum/s4/native-characterization/s4-native-family-map.review-base.json
benchmarks/corpora/vegeta-ethereum/s4/native-characterization/s4-review-decisions.draft.json
```

The review-base map is initialized once from the generated bootstrap candidate and is never overwritten by later characterization runs. If a reviewed S4 family needs a genuinely new native implementation/profile (for example the custom router), add that native-family definition to the review-base map; ordinary decisions that reuse an existing native family only need the decisions file.

The six seeded rows are the first high-impact S4 families (custom router, LINK, SEEK, cbETH, RNDR, MUBI). The repository also contains a checked-in first reviewed batch plus the S4-only conservative custom-router family alias. To install those six reviewed decisions into the generated workspace and immediately recompute exact coverage, run:

```bash
bash tools/vegeta/run-vegeta-s4-apply-first-batch.sh
```

The wrapper does not freeze the map: it runs the exact review check with low-coverage reporting enabled. The repository also contains a second conflict-closure batch with five conservative existing-native family aliases (two ERC721 families, CTSI, SHIB, and L3E7 Worlds) plus explicit pending scaffolding for AMP, Linea, zkSync, Arbitrum, and the next unknown high-conflict families. After the first batch, install that batch with `bash tools/vegeta/run-vegeta-s4-apply-second-batch.sh`; pending rows remain non-executable. For additional families you actually review, set:

```json
"review_status": "reviewed",
"reviewed_native_family": "...",
"mapping_basis": "concrete source/interface/selector review evidence"
```

Then recompute the reviewed draft and the exact conflict freeze gates:

```bash
bash tools/vegeta/run-vegeta-s4-review-check.sh
```

The family map cannot be frozen until all of these pass:

```text
source conflict-pair coverage                 >= 95%
median conflict-bearing block coverage        >= 80%
frozen corpus internal integrity              PASS
```

Conflict-relevant storage-access, all-storage/state-owner, and fully mapped transaction-gas coverage remain reported diagnostics. They do not gate family freeze because S4 is a conflict/dependency workload, not an EVM application/state-equivalence claim. After the first batch, run `run-vegeta-s4-apply-second-batch.sh`; it applies only the five checked-in existing-native aliases, preserves all pending bridge/system rows, recomputes exact coverage, and writes `conflict-closure.{json,txt}`. If conflict coverage is still below 95%, continue only with the top remaining conflict families in that report / `family-review-queue.md`. Do not chase access-only long-tail state to satisfy the family freeze.
The checked-in third conflict-closure batch can then be installed with `bash tools/vegeta/run-vegeta-s4-apply-third-batch.sh`. It promotes only two previously pending token rows after additional frozen-selector review, adds reviewed standard token aliases, and adds one conservative `v3-pool-lock` alias for the observed MUBI/WETH Uniswap-V3 swap family. AMP, Linea, zkSync, Arbitrum, and other bridge/system families are deliberately left pending. The wrapper recomputes exact unique conflict coverage and tells you whether the family map may be frozen.

When the gate passes and you have personally reviewed the draft, freeze it explicitly:

```bash
VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh
```

This writes:

```text
evaluation/vegeta/s4-native-family-map.v1.json
```

with hashes of the provenance/coverage/freeze evidence. `candidate_only=false` cannot be produced by the normal workflow without this human-attested freeze step.

Then prepare the executable Wasmd bundle:

```bash
bash tools/vegeta/run-vegeta-s4-prepare-native.sh
```

Preparation recomputes family coverage and enforces family/selector-reviewed conflict coverage, median conflict-bearing-block coverage, reviewed-state transaction/conflict-participant coverage, and implementation readiness. Conflict-relevant/all-storage/state-owner and source-state gas statistics remain diagnostics rather than hard gates. It builds/validates native Wasm, reconstructs predecessor logical state, validates the streamed execution plan, and materializes S4-local symbolic profiles under `s4/native-execution/symbolic/`.

`evaluation/workloads/prepare_s4.sh` is the convenience artifact wrapper. S4 intentionally has no exact per-transaction SLOAD/SSTORE oracle; exact-access headroom remains S3-only.
