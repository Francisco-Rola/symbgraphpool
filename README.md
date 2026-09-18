# Beacon

Beacon is a Rust/CosmWasm research runtime for speculative smart-contract execution across the consensus boundary. Symbolic profiles predict conflicts, the runtime pre-executes transactions while ordering is in progress, and concrete validation plus selective replay allow valid speculative results to be reused after the canonical order is known.

## Start here

```bash
bash scripts/test-all.sh
bash evaluation/eurosys/cluster-preflight.sh
```

Run evaluation stages directly from `evaluation/experiments/`, or use `evaluation/eurosys/run-cluster-paper.sh` for the complete cluster campaign. See [`evaluation/README.md`](evaluation/README.md) and [`evaluation/eurosys/README.md`](evaluation/eurosys/README.md) for workload and cluster instructions.

## Repository map

```text
crates/       symbolic profiles, candidate graph, adaptive feedback and scheduling
runtime/      speculative execution, MVCC, reconciliation and Rust/Go bridge
benchmarks/   CosmWasm contracts, Wasmd evaluator and symbolic profiles
evaluation/   workloads, experiment drivers, cluster runners and plotting
tools/        preparation, validation and summarization utilities
scripts/      repository-wide validation
docs/         architecture and implementation summary
```

See [`docs/implementation-summary.md`](docs/implementation-summary.md) for the current system design.
