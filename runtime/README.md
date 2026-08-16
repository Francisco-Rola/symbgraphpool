# Runtime

The runtime workspace contains ACG's execution and evaluation path.

## Main crates

- `acg-cosmwasm-engine` — CosmWasm/native execution, detached receipts, MVCC and READY-DAG metrics.
- `acg-cosmwasm-adapter` — execution-request to graph-transaction adapter.
- `acg-runtime-feedback` — adaptive planning and conflict/replay/serialization feedback.
- `acg-validator-sim` — deterministic ingress, block production, speculative execution and replay.
- `acg-evaluation` — stable experiment records, manifests and Phase-5F acceptance.
- `acg-benchmark-harness` — workload-independent serial/speculative benchmark pipeline.

## Validate

```bash
./scripts/run-all-tests.sh
```

For performance work use the current ConflictLab runners from the repository root:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
./scripts/run-conflictlab-parallelism-evaluation.sh
```
