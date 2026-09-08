# Vegeta workload tooling

This directory owns source-trace collection, frozen-family characterization, native-plan construction, validation, and summarization helpers for Vegeta-derived workloads. `evaluation/` contains the paper-facing experiment entrypoints; translation internals stay here.

## S4 after source collection

Once `run-vegeta-s4-native-inputs.sh` reports that S4 native-characterization inputs are frozen, run:

```bash
bash tools/vegeta/run-vegeta-s4-characterize.sh
```

This stage is local-only. It validates the 5,000-block range, writes `corpus-provenance.{json,txt}`, records (but does not normalize away) any difference from Vegeta's published S4 transaction count, ranks runtime families/selectors, bootstraps only identical bytecode mappings already reviewed for S1/S3, and computes exact source conflict coverage plus all-storage, conflict-relevant/state-owner, and conservative all-or-nothing transaction-gas metrics. It also writes `family-blocker-clusters.json` and `family-marginal-coverage.{json,txt}`. The planner emits balanced, access-first, and conflict-first dual-gate plans with exact conflict-union recomputation. All-storage-access and exact source-conflict coverage are hard publication/family-freeze gates; conflict-relevant access and strict-gas views remain transparent diagnostics and never become executable mappings automatically.

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

Then recompute the reviewed draft and the exact dual publication/freeze gates:

```bash
bash tools/vegeta/run-vegeta-s4-review-check.sh
```

The family map cannot be frozen until all of these pass:

```text
all source storage-access coverage            >= 90%
source conflict-pair coverage                 >= 95%
median conflict-bearing block coverage        >= 80%
frozen corpus internal integrity              PASS
```

All-storage-access coverage is a hard family-freeze gate. Conflict-relevant access, state-owner occurrence, and fully mapped transaction-gas coverage remain diagnostics. After the first batch, run `run-vegeta-s4-apply-second-batch.sh`; it applies only the five checked-in existing-native aliases, preserves all pending bridge/system rows, recomputes exact coverage, and writes `conflict-closure.{json,txt}`. If either the 90% all-storage or 95% conflict target remains open, continue with the balanced dual-gate planner; once conflict closes, continue with access-heavy reviewed families until the storage gate also closes.
The checked-in third conflict-closure batch can then be installed with `bash tools/vegeta/run-vegeta-s4-apply-third-batch.sh`. It promotes only two previously pending token rows after additional frozen-selector review, adds reviewed standard token aliases, and adds one conservative `v3-pool-lock` alias for the observed MUBI/WETH Uniswap-V3 swap family. AMP, Linea, zkSync, Arbitrum, and other bridge/system families are deliberately left pending. The wrapper recomputes exact unique conflict coverage and tells you whether the family map may be frozen.
The checked-in fourth conflict batch is installed with `bash tools/vegeta/run-vegeta-s4-apply-fourth-batch.sh`. It requires the third batch to be present, validates checked-in public-source evidence for every P12-P16 decision, promotes AMP/Linea/zkSync/Arbitrum to distinct conservative dependency-lock aliases, promotes the 31-owner Archetype ERC721 runtime family to `cw721-drop`, recomputes an exact pre-batch baseline and post-batch coverage, and writes `s4-fourth-batch-coverage-delta.{json,txt}`. The wrapper never freezes automatically; if either hard gate remains open, continue from the regenerated balanced/access-first plan.

After the fourth batch, use `bash tools/vegeta/run-vegeta-s4-plan-fifth-batch.sh` for the long tail. This is a dedicated wide pass (2000 planner steps / 2000 reported families by default) that writes `s4-fifth-batch-review-candidates.{json,txt}` and a pending `s4-fifth-batch-review-decisions.draft.json`. The conflict shortlist is the projected minimum conflict-first prefix that crosses 95%; the access list is kept separate. Then run `bash tools/vegeta/run-vegeta-s4-analyze-fifth-batch.sh`. The analysis pass scans the frozen corpus and complete callTracer cache once and writes `s4-fifth-batch-evidence.{json,md}` with every observed owner for each shortlisted family, exact direct/call-frame selectors, storage read/write activity, representative transactions, DELEGATECALL/CALLCODE targets, EIP-1167 hints, bytecode fingerprints, existing delegate-resolution evidence, and blocker/co-blocker context. Set `VEGETA_S4_FIFTH_FETCH_SOURCE=1` to add Sourcify verified-contract metadata (and Etherscan v2 fallback when `ETHERSCAN_API_KEY` is set). This evidence artifact is analysis-only and never creates a mapping. Anonymous hashes remain pending until a reviewer records source/interface evidence, a semantic conclusion, a native family, and a mapping basis. The checked-in fifth review subset deliberately reviews only evidence-supported rows and leaves insufficient-evidence rows unresolved/non-executable; `bash tools/vegeta/run-vegeta-s4-apply-fifth-batch.sh` validates those conclusions against the freshly generated local dossier before merging them and writes an exact before/after delta. Generic proxy runtime hashes are not application identities: owner-scoped delegate implementation profiles are used where needed. Explicitly unresolved/deferred/rejected families are skipped on subsequent shortlist generation using conservative post-skip conflict accounting, and an existing human-editable decisions draft is preserved instead of making the planning command fail. If the conflict gate remains open, rerun the wide planner rather than forcing unresolved rows; if conflict closes but storage remains below 90%, switch to the access-heavy list.

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

Preparation recomputes family coverage and enforces the 90% all-storage family gate again, family/selector-reviewed conflict coverage, median conflict-bearing-block coverage, reviewed-state transaction/conflict-participant coverage, and implementation readiness. Conflict-relevant/state-owner and source-state gas statistics remain diagnostics. It builds/validates native Wasm, reconstructs predecessor logical state, validates the streamed execution plan, and materializes S4-local symbolic profiles under `s4/native-execution/symbolic/`.

`evaluation/workloads/prepare_s4.sh` is the convenience artifact wrapper. S4 intentionally has no exact per-transaction SLOAD/SSTORE oracle; exact-access headroom remains S3-only.
