# Vegeta workload tooling

This directory owns source-trace collection, frozen-family characterization, native-plan construction, validation, and summarization helpers for Vegeta-derived workloads. `evaluation/` contains the paper-facing experiment entrypoints; it should not duplicate translation internals.

## S4 after source collection

Once `run-vegeta-s4-native-inputs.sh` reports that the S4 native-characterization inputs are frozen, use:

```bash
bash tools/vegeta/run-vegeta-s4-characterize.sh
```

This is local-only. It validates the 5,000-block S4 range, ranks runtime families/selectors, reuses only identical runtime-code mappings already reviewed for S1/S3 as a **candidate**, measures their S4 conflict coverage, and writes `native-characterization/family-review-queue.md`.

Review any high-impact unmapped families/selectors and freeze the reviewed map as:

```text
evaluation/vegeta/s4-native-family-map.v1.json
```

Then prepare the executable Wasmd bundle:

```bash
bash tools/vegeta/run-vegeta-s4-prepare-native.sh
```

The preparation step recomputes family and selector-aware semantic coverage, enforces readiness gates, builds/validates the required native Wasm artifacts, reconstructs predecessor logical state, validates the execution plan, and materializes the workload-local symbolic profile bundle under `s4/native-execution/symbolic/`.

`evaluation/workloads/prepare_s4.sh` is the convenience artifact wrapper: if no reviewed map exists it runs characterization and stops; otherwise it delegates to the native preparation step.

S4 intentionally has no exact per-transaction SLOAD/SSTORE oracle. Exact-access headroom remains an S3-only experiment.
