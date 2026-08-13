# Next implementation work after Brick 5C.7

Brick 5C.7 is the current production execution substrate: dependency-driven READY-DAG speculative execution over block-local persistent MVCC, followed by canonical receipt validation/reuse/replay.

The next production work is **not** another VM pool or cache-sharding strategy. Those experiments are archived under `research/vm-lifecycle/` and are intentionally absent from the production engine.

## Near-term priorities

1. **Brick 5F acceptance matrix** — define stable serial-equivalence, replay/reuse, and performance acceptance gates for ConflictLab, MiniWarehouse, and controlled synthetic workloads.
2. **Brick 5E production metrics** — productize only the metrics that are useful for diagnosing exposed parallelism, observed service inflation, and scheduler realization without carrying research policy knobs into the engine API.
3. **MiniWarehouse scan/iterator diagnosis** — if workload-specific service inflation remains significant, isolate range/iterator behavior separately from point MVCC.
4. **Granularity-aware execution policy research** — measure whether fewer workers help very small transactions; keep any policy advisory and correctness-independent.
5. **Brick 5D broader adaptive learning** — extend validation/replay-driven feedback once the execution/performance baseline is stable.

## Separate VM research branch

A correct pristine VM snapshot/reset/copy-on-write mechanism remains a potentially high-value optimization. It requires state-reset semantics below the current public `cosmwasm_vm::Instance` abstraction and must not enter production until adversarial isolation tests cover memory, mutable globals, tables, gas, memory growth, traps/out-of-gas, storage/querier rebinding, and end-to-end world-state equivalence.

See [`implementation-status.md`](implementation-status.md) and [`../research/vm-lifecycle/README.md`](../research/vm-lifecycle/README.md).
