# ConflictLab evaluation

ConflictLab is the controlled ground-truth workload used to find ACG's break-even points before
adding external benchmarks. Release matrices use the real CosmWasm contract (`execution_backend=wasm`).

## Transaction complexity

All tiers execute the same logical `Credit(account)` conflict pattern while varying non-state-changing
compute, repeated read/write rounds on the same account key, and message payload size:

| tier | compute iterations | storage rounds | payload bytes |
|---|---:|---:|---:|
| tiny | 0 | 0 | 0 |
| light | 4,096 | 1 | 64 |
| medium | 32,768 | 2 | 256 |
| heavy | 262,144 | 4 | 1,024 |
| very-heavy | 1,048,576 | 8 | 4,096 |

These are calibration tiers, not claims that a tier equals a particular production contract.

## Matrices

- `quick.grid.json`: 72-run release sanity/tuning sample.
- `granularity.grid.json`: 1,350-run complexity × contention × 1–6-worker matrix.
- `contention.grid.json`: 1,260-run account-cardinality/hotspot sweep.
- `block-scaling.grid.json`: 300-run block-size/planner-scaling sweep.
- `phase-change.grid.json`: 120-run low↔high contention and cheap↔expensive phase sweep.
- `ingress-block.grid.json`: 720-run admission-TPS/block-window/block-size packing sweep.
- `policy-tuning.grid.json`: 260-run one-factor-at-a-time scheduler/feedback tuning sweep after baselines are known.

Run one matrix:

```bash
./scripts/run-conflictlab-release-matrix.sh evaluation/conflictlab/granularity.grid.json
```

Run a campaign:

```bash
./scripts/run-conflictlab-release-suite.sh quick   # start here
./scripts/run-conflictlab-release-suite.sh core
```

Do not tune policy constants from debug builds or single repetitions. Use accepted release records
and the generated `plot-long.csv`.
