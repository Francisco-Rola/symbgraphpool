# Phase 5F — formal acceptance and reproducible evaluation gates

Phase 5F freezes the boundary between a raw performance sample and an evaluation result that is
admissible for scientific comparison. It does not change transaction execution, adaptive planning,
canonical validation, replay, or consensus-visible state.

## 5F.1 — publication-grade run acceptance

`acg-evaluation` now exposes `AcceptancePolicy::publication()`. A publication record must carry:

- stable experiment/workload/mode/run/seed identity;
- UTC start time, Git revision, build profile, Rust compiler version;
- CPU model, logical-core count, OS, kernel, and Git dirty-tree status;
- a clean Git worktree and the manifest-required build profile (publication defaults to `release`);
- explicit workload parameters;
- a worker count within both the recorded and manifest physical-core budget;
- serial-equivalent work, serial-cost DAG, observed-service DAG, service inflation, and scheduler
  realization;
- canonical and serial-reference state digests with an explicit serial-equivalence result;
- internally consistent candidate/dependency class counts and timing totals.

Failure categories are explicit: incomplete provenance/measurements, correctness failure,
configuration error, or performance regression.

A relaxed `AcceptancePolicy::smoke()` exists for mechanism tests and CI tests whose purpose is not a
publication-quality measurement.

## 5F.2 — correctness digests and provenance capture

`CorrectnessRecord::from_state_bytes` SHA-256 hashes deterministic canonical-state encodings and
records whether serial and adaptive executions are identical.

`ExperimentMetadata::capture_standard_environment` performs best-effort capture of:

- RFC3339 UTC start time;
- Git revision and dirty-tree status;
- `rustc --version`;
- build profile when provided through `ACG_BUILD_PROFILE`/`PROFILE`;
- OS, architecture, kernel, logical-core count, CPU model, and total Linux memory when available.

Missing metadata is not replaced with invented values. Publication acceptance reports it as
incomplete.

## 5F.3 — explicit experiment manifests

`ExperimentManifest` schema v1 enumerates every expected run by:

```text
workload + mode + run_index + seed + workers + exact parameters
```

The manifest also freezes the expected `ExperimentRecord` schema, physical-core ceiling, and
acceptance policy. Duplicate run identities and manifest oversubscription are rejected before
results are interpreted.

When a JSONL result set is checked, Phase 5F reports missing, unexpected, and duplicate samples.
This prevents accidental cherry-picking or silently incomplete experiment matrices.

## 5F.4 — optional CI performance gates

Scientific validity does not imply speedup. By default, publication acceptance has no hard-coded
performance target. Experiments or CI lanes may explicitly configure ceilings/floors for:

- service inflation;
- scheduler realization;
- planning overhead relative to execution wall;
- feedback-update overhead relative to execution wall;
- minimum serial-equivalent/parallel speedup.

A record that is complete and correct but violates an explicitly configured threshold is classified
`performance_regression`, not `correctness_failure`.

## 5F.5 — machine-readable acceptance reports

`ExperimentAcceptanceReport` schema v1 contains aggregate status, accepted/rejected counts, missing,
unexpected, and duplicate run identities, and per-run issues plus derived overhead/speedup ratios.
Reports serialize deterministically to JSON.

The `acg-evaluate` binary and `tools/internal/validate-experiment-records.sh` provide the common command-line
gate for future ConflictLab, MiniWarehouse, and external benchmark campaigns.

## Focused validation

Phase-5F acceptance regressions and the full source/test/lint checkpoint are both covered by:

```bash
./scripts/test-all.sh
```

Long benchmark campaigns remain separate from per-patch validation.

The focused tests cover:

1. complete publication records and manifest/report JSON round trips;
2. distinct incomplete, correctness-failure, configuration-error, and performance-regression
   classifications;
3. missing, unexpected, and duplicate sample detection;
4. explicit optional performance thresholds;
5. deterministic correctness digests and JSONL reading;
6. relaxed smoke acceptance for mechanism-only records;
7. host metadata capture without overwriting explicit fields;
8. the existing Phase 5E runtime measurement/record integration and schema tests.

## What 5F intentionally does not do

- It does not select benchmarks or claim paper thresholds before the benchmark matrix exists.
- It does not make wall-clock evidence consensus visible.
- It does not silently fill missing provenance.
- It does not decide correctness from learned conflict/risk models.

After Phase 5F, the next main implementation task is the common benchmark adapter/harness and the
controlled ConflictLab evaluation matrix, followed by MiniWarehouse and external workloads.
