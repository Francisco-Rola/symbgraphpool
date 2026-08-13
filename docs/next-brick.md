# Next implementation work after the common benchmark harness

Brick 5A through 5F and the manifest-driven common benchmark harness are now implemented. The
execution substrate and measurement/acceptance schemas should remain stable while the project moves
into controlled evaluation and tuning. VM reset/pool/cache-shard work remains research-only under
`research/vm-lifecycle/`.

## 1. ConflictLab ground-truth matrix + oracle baseline

Use the harness to generate a declared matrix over:

- conflict probability/locality and hotspot skew;
- transaction service cost;
- worker count from 1 through the physical-core budget;
- feedback retention/confidence;
- hard/soft/risk thresholds;
- block size;
- phase-history/warm-up length.

Preserve `static`, `probability-only`, and `cost-aware` as explicit ablations. Add an oracle-conflict
baseline for ConflictLab so learned schedules can be compared against known concrete dependency
structure.

## 2. MiniWarehouse adapter

Implement `BenchmarkWorkload`/`PreparedBenchmark` for MiniWarehouse using exactly the same lifecycle:
independent serial setup, identical generated blocks, deterministic canonical-state encoding, Brick
5E record, and Brick 5F acceptance. No benchmark-specific timing schema should be added. Sweep
warehouses, block size, transaction mix, remote-stock probability, skew, workers, and warm-up blocks.

## 3. Sweep generation and statistical layer

Add utilities that generate manifests from parameter grids and aggregate accepted JSONL without
rewriting records. Freeze repeated-run methodology, mode-order randomization/control, confidence
intervals, raw-sample retention, and deterministic table/figure generation before the large campaign.

## 4. Bottleneck-driven optimization

Only optimize after accepted matrices identify a material bottleneck. The first candidates to test
explicitly are:

- candidate-graph/planner scaling versus block size and edge density;
- point versus iterator/range-heavy MVCC service inflation;
- transaction granularity and 1..physical-core worker scaling;
- VM lifecycle only if accepted external/application workloads still show it as dominant.

## 5. External workloads

After ConflictLab and MiniWarehouse use the same accepted pipeline, add independently designed
workloads with different conflict structures. New workloads should require only an adapter plus
manifest parameters, not changes to READY-DAG, feedback, records, or acceptance semantics.

See [`common-benchmark-harness.md`](common-benchmark-harness.md), [`brick-5f.md`](brick-5f.md),
[`brick-5e.md`](brick-5e.md), and [`implementation-status.md`](implementation-status.md).
