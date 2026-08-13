# Next implementation work after Brick 5D

Brick 5D now closes the adaptive planning loop: concrete reconciliation identifies why a receipt replayed, measured replay cost/fan-out are persisted with decay, and future candidate edges expose a cost-adjusted scheduling risk without changing canonical correctness.

The next production work should be **Brick 5E followed by Brick 5F**. VM reset/pool/cache-shard work remains research-only under `research/vm-lifecycle/`.

## 1. Brick 5E — productize cost/efficiency metrics and policy inputs

The 5D policy currently uses a configured `serialization_cost_reference_nanos`. Brick 5E should replace that blunt reference with stable measurements or a conservative estimator of lost parallelism, while exposing a compact diagnostics surface:

- raw conflict probability and confidence;
- cost-adjusted scheduling risk and cost confidence;
- expected direct replay cost and expected replay fan-out;
- actual replay work and cascade work;
- serial-cost DAG, observed-service DAG, service inflation, and scheduler realization;
- planner/feedback overhead.

The goal is not to make wall-clock timing consensus-visible. All timing remains validator-local optimization evidence.

## 2. Brick 5F — acceptance matrix

Turn the existing correctness/performance suite into explicit gates for:

- serial-equivalent final state and outputs;
- receipt reuse/replay correctness for points, ranges, balances, metadata, traps/failures, and nested calls;
- 5D attribution conservation and checkpoint v1/v2 compatibility;
- phase-change adaptation (soften → expensive replay harden → decay/recover);
- bounded planner/feedback overhead;
- READY-DAG realization on controlled 6/4/2/1-lane workloads;
- MiniWarehouse replay/reuse and end-to-end wall-time targets on the six-core reference machine.

## Parallel research tracks

- **VM snapshot/reset/COW:** potentially high value, but remains outside production until full isolation/equivalence gates pass.
- **MiniWarehouse iterator/range scans:** point MVCC is largely exonerated; isolate scan/iterator costs if workload-specific inflation remains high.
- **Granularity-aware worker policy:** measure 1–6 worker behavior for tiny transactions before adding any adaptive concurrency hint.
- **Soft-edge exploration:** later consider a deterministic/local exploration policy so heavily serialized relationships can be periodically re-measured without affecting correctness.

See [`brick-5d.md`](brick-5d.md) and [`implementation-status.md`](implementation-status.md).
