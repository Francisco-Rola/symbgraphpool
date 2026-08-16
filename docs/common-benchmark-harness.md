# Common benchmark harness

The harness turns one Phase-5F `RunIdentity` into one measured block plus an independent serial
reference. Workload setup is performed twice and must generate identical initial state and blocks.
Final serial/speculative state digests must match.

## Workload boundary

`BenchmarkWorkload` prepares deterministic runs; `PreparedBenchmark` exposes the engine, profile
graph, warm-up blocks, measured block, canonical-state bytes and workload environment metadata.
Adding a workload should not change READY-DAG/MVCC, evaluation records, or acceptance logic.

Built in today: ConflictLab. MiniWarehouse is next.

## Modes

- `static`: no feedback retained;
- `probability-only`: conflict/topology learning only;
- `cost-aware`: full replay/fan-out/serialization-cost learning.

All modes use the same speculative executor and canonical validation/replay path.

## Run

```bash
./scripts/run-benchmark-manifest.sh manifest.json
```

For current ConflictLab evaluation use:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
./scripts/run-conflictlab-parallelism-evaluation.sh
```

Parameter definitions live in `docs/tuning-knobs.md`. Matrix generation/aggregation is documented in
`evaluation/README.md`.
