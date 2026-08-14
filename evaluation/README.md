# Evaluation

Evaluation is manifest-driven. A declared run is executed with an independent serial reference and
one speculative policy, produces a stable `ExperimentRecord`, and must pass Brick-5F acceptance.

## One manifest

```bash
./scripts/run-benchmark-manifest.sh evaluation/conflictlab-harness-smoke.json
```

## Generate a matrix

```bash
python3 scripts/generate-manifest-matrix.py \
  evaluation/conflictlab/quick.grid.json /tmp/conflictlab.json
```

A matrix expands modes, workers, seeds, parameter grids and linked complexity cases into exact run
identities. Unknown workload/tuning parameters are rejected by the harness.

## Aggregate results

```bash
python3 scripts/aggregate-experiment.py records.jsonl --out-dir aggregate
```

Outputs:

- `records-flat.csv`: one flattened row per raw sample;
- `summary-wide.csv`: one grouped row with metric statistics;
- `plot-long.csv`: tidy/plot-ready metric rows, including corrected scheduler realization, dependency compression and feedback/serialization batching factors;
- `summary.json`: record/group counts and exported metric list.

Raw JSONL is always the source of truth. Confidence intervals use the normal 1.96×SEM approximation;
keep raw samples for any later bootstrap/non-parametric analysis. Matrix files may set `order_seed` to
deterministically shuffle run order and reduce systematic thermal/order bias.

## Release ConflictLab campaign

```bash
./scripts/run-conflictlab-release-suite.sh quick
./scripts/run-conflictlab-release-suite.sh core
./scripts/run-conflictlab-release-suite.sh full
```

`quick` validates the release pipeline; `core` covers the primary research axes; `full` additionally
covers admission/block packing and scheduler-policy tuning. See `conflictlab/README.md` and
`../docs/tuning-knobs.md`.

## Control-plane/adaptation calibration

Before the large ConflictLab matrices, validate the dense-graph corrections and exercise real
adaptive decisions:

```bash
./scripts/run-control-plane-corrections-diagnostics.sh
./scripts/run-conflictlab-control-plane-evaluation.sh
```

ExperimentRecord schema v2 adds dependency-reduction/batching counters and a worker-capacity-aware
scheduler lower bound while retaining schema-v1 read compatibility.
