# Adaptive Conflict Graph

Adaptive Conflict Graph (ACG) is a Rust/CosmWasm research runtime for speculative smart-contract execution across the consensus boundary. Offline symbolic profiles predict conflicts, Rust builds a compact risk-aware dependency plan, Wasmd/WasmVM preexecutes dependency-ready transactions, and concrete reconciliation plus selective replay preserve serial-equivalent state.

## Start here

```bash
bash scripts/test-all.sh
PAPER_EVAL_PROFILE=debug bash evaluation/experiments/run-all.sh
```

The paper artifact, workload preparation, metrics and figure plan live in [`evaluation/README.md`](evaluation/README.md) and [`evaluation/PAPER_PLAN.md`](evaluation/PAPER_PLAN.md). Publication entry points are only under `evaluation/experiments/`; historical preparation helpers under `tools/legacy-scripts/` are internal dependencies, not experiment interfaces.

## Repository map

```text
crates/       symbolic profiles, candidate graph, adaptive feedback and scheduling
runtime/      speculative execution, MVCC, reconciliation and Rust/Go bridge
benchmarks/   CosmWasm contracts, Wasmd evaluator and prepared symbolic profiles
evaluation/   canonical workloads, experiments, plotting and paper plan
tools/        preparation, validation and summarization utilities
scripts/      repository-wide validation only
docs/         current architecture and implementation summary
```

See [`docs/implementation-summary.md`](docs/implementation-summary.md) for the current system design.
