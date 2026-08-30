# Evaluation

Evaluation is manifest-driven. Every measured run builds an independent serial reference, emits a
schema-v3 `ExperimentRecord`, and passes the Phase-5F acceptance policy before it is reported.

## Main entrypoints

ConflictLab 1.0 — broad correctness/mechanism evaluation:

```bash
./tools/legacy-scripts/run-conflictlab-v1-evaluation.sh
```

Controlled six-worker parallelism ceiling + overhead attribution:

```bash
./tools/legacy-scripts/run-conflictlab-parallelism-evaluation.sh
```

Run one explicit manifest:

```bash
./tools/legacy-scripts/run-benchmark-manifest.sh manifest.json [output-directory]
```

`records.jsonl` is the source of truth. Aggregated CSVs and text reports are derived artifacts.
Implementation helpers live under `tools/internal/`; they are not intended as user-facing
entrypoints.

See `conflictlab/README.md` for the active workload axes and experiment definitions.
