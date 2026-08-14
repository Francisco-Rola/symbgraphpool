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

## Prediction quality

`prediction_quality` separates executor/contention experiments from adaptive-learning experiments:

- `exact`: current precise account-key refinement;
- `coarse`: account-family candidate edges remain unresolved, allowing soft-edge speculation;
- `opaque`: the visible account is a decoy and the real account is recovered inside Wasm from the
  payload, deliberately creating candidate misses.

`coarse` and `opaque` are controlled perturbations for measuring adaptation; they are not assertions
about production analyzer accuracy.

## Matrices

- `quick.grid.json`: 72-run release sanity/tuning sample.
- `granularity.grid.json`: 1,350-run complexity × contention × 1–6-worker matrix.
- `contention.grid.json`: 1,260-run account-cardinality/hotspot sweep.
- `block-scaling.grid.json`: 360-run production-sized 16/32/64/128/256/512 block/planner sweep.
- `phase-change.grid.json`: 120-run low↔high contention and cheap↔expensive phase sweep.
- `ingress-block.grid.json`: 720-run admission-TPS/block-window/block-size packing sweep.
- `policy-tuning.grid.json`: 260-run one-factor-at-a-time scheduler/feedback tuning sweep after baselines are known.
- `control-plane-regression.grid.json`: 108-run exact-prediction production block-size sweep (16/32/64/128/256/512) for dependency reduction, upstream feedback aggregation, corrected worker bounds, and full adaptive-pipeline timing.
- `forced-speculation.grid.json`: 432-run **coarse-prediction** risk-budget sweep over the same production block sizes, 25%/75% hotspot contention, 0/4 warm-up blocks, and risk budgets 0.50/0.75/0.90 so learned policy changes can alter actual waves/dependencies.

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

## Control-plane correction campaign

After applying the upstream-aggregation/pipeline-timing/schema-v3 patch, run:

```bash
./scripts/run-control-plane-corrections-diagnostics.sh
./scripts/run-conflictlab-control-plane-evaluation.sh
```

The second command runs both focused release matrices (108 exact-scaling + 432 coarse-policy runs),
aggregates them, and writes `results-summary.txt`. For shorter iterations, run
`run-conflictlab-production-scaling-evaluation.sh` or
`run-conflictlab-coarse-policy-risk-evaluation.sh` independently. Upload `results-summary.txt`
together with `summary.txt`, `records.jsonl`, `aggregate/summary-wide.csv`, and
`aggregate/plot-long.csv` for analysis. All focused measured blocks are capped at 512 transactions.
