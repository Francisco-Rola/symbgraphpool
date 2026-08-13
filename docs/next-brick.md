# Next implementation work after Brick 5E

Brick 5E productizes the performance evidence required by the closed-loop Brick 5D policy: it
learns per-relationship marginal serialization cost from READY-DAG timings and emits one stable
machine-readable experiment schema for future workloads.

The next production milestone should be **Brick 5F — formal acceptance and reproducible evaluation
gates**. VM reset/pool/cache-shard work remains research-only under `research/vm-lifecycle/`.

## 1. Brick 5F — acceptance matrix

Turn the existing correctness/performance suite into explicit, automatically reported gates for:

- serial-equivalent final state and outputs;
- receipt reuse/replay correctness for points, ranges/iterators, balances, metadata, nested calls,
  traps/failures, and out-of-gas behavior;
- 5D attribution conservation, replay fan-out, and phase-change adaptation;
- feedback checkpoint v1/v2/v3 compatibility;
- 5E marginal serialization-cost learning and fallback behavior;
- stable experiment-schema version/required metadata;
- bounded planner + feedback overhead;
- READY-DAG realization on controlled 6/4/2/1-lane workloads;
- worker scaling from one through the physical-core budget;
- MiniWarehouse serial equivalence, replay/reuse, and end-to-end wall-time targets.

Brick 5F should produce a compact PASS/FAIL acceptance report in addition to raw JSONL records.

## 2. Benchmark-ready harness after 5F

Once metric definitions and gates are frozen, introduce a workload adapter that can run
ConflictLab, MiniWarehouse, and future external workloads through the same experiment lifecycle:

```text
setup -> deterministic transactions -> serial reference -> adaptive run
      -> correctness digest -> ExperimentRecord JSONL
```

Then extend ConflictLab into the ground-truth parameter sweep for conflict probability, skew,
transaction cost, fan-out, DAG width/depth, and phase changes before adding external benchmarks.

## Parallel research tracks

- **VM snapshot/reset/COW:** potentially high value, but outside production until full
  isolation/equivalence gates pass.
- **MiniWarehouse iterator/range scans:** point MVCC is largely exonerated; isolate scan/iterator
  costs if workload-specific inflation remains high.
- **Granularity-aware worker policy:** measure 1–6 worker behavior for tiny transactions before
  adding any adaptive concurrency hint.
- **Soft-edge exploration:** later consider a deterministic/local exploration policy so heavily
  serialized relationships can be periodically re-measured without affecting correctness.

See [`brick-5e.md`](brick-5e.md), [`brick-5d.md`](brick-5d.md), and
[`implementation-status.md`](implementation-status.md).
