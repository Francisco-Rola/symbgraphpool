# Adaptive Conflict Graph

Adaptive Conflict Graph (ACG) is a Rust/CosmWasm research runtime for conflict-aware speculative
transaction execution. It builds a symbolic candidate graph, schedules a READY-DAG, executes against
block-local MVCC state, validates in canonical order, selectively replays invalid work, and feeds
runtime evidence back into later scheduling decisions.

## Implemented

- **Phases 1–2:** symbolic profiles and concrete transaction candidate graphs.
- **Phases 3–4:** runtime conflict feedback, learned probabilities, and risk-bounded scheduling.
- **Phase 5:** detached speculative receipts, canonical validation/replay, READY-DAG + MVCC,
  replay/serialization-cost feedback, stable experiment records, and acceptance gates.
- **Benchmark harness:** deterministic serial reference plus static, probability-only, and cost-aware
  policy runs.
- **ConflictLab 1.0:** 15-campaign real-Wasm correctness/performance suite on six workers.

VM lifecycle experiments that are not part of the normal runtime remain under `research/vm-lifecycle/`.

## Main commands

Run the repository gate after code changes:

```bash
./scripts/run-all-tests.sh
```

Run the complete ConflictLab 1.0 suite:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

Run the focused six-worker parallelism-ceiling/overhead experiment:

```bash
./scripts/run-conflictlab-parallelism-evaluation.sh
```

The long evaluations are intentionally separate from the normal test gate.

## Repository map

```text
crates/       symbolic graph, candidate graph, predictor and scheduler
runtime/      CosmWasm engine, MVCC/READY-DAG execution, feedback and evaluation harness
benchmarks/   ConflictLab and MiniWarehouse contracts + symbolic profiles
evaluation/   current experiment definitions and acceptance policies
scripts/      current test/evaluation entrypoints and their small helper tools
docs/         architecture, phase notes and tuning documentation
research/     quarantined experiments
```

See `evaluation/conflictlab/README.md` for the active ConflictLab experiments and
`docs/implementation-status.md` for the current implementation snapshot.
