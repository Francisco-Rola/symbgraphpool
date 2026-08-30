# Adaptive Conflict Graph

Adaptive Conflict Graph (ACG) is a Rust/CosmWasm research runtime for conflict-aware speculative
transaction execution. Offline symbolic entrypoint profiles build a persistent conflict model;
blocks instantiate an atomic transaction candidate graph; runtime feedback refines conflict risk;
Rust emits a reduced dependency-ready DAG; Wasmd/WasmVM executes speculatively; canonical validation
and selective replay preserve serial-equivalent state.

## Maintained commands

Run the complete repository validation gate:

```bash
bash scripts/test-all.sh
```

Run the controlled Wasmd comparison locally (2/4/6 workers on a six-core host):

```bash
bash scripts/eval-wasmd-debug.sh
```

Run the publication-style single-machine campaign from a clean committed tree:

```bash
bash scripts/eval-wasmd-paper.sh
```

The maintained comparison is Serial vs Cosmos BlockSTM vs AriaFB-style vs Vegeta-style vs Rust-ACG,
all on the same Wasmd/WasmVM/Cosmos SDK state machine. See `evaluation/wasmd/README.md` for metric
definitions and the publication roadmap.

## Repository map

```text
crates/       symbolic profiles, predicates, candidate graph, adaptive feedback and scheduler
runtime/      CosmWasm runtime/executors, MVCC, reconciliation, FFI and benchmark harnesses
benchmarks/   contracts, Wasmd comparison machinery and symbolic profile corpus
evaluation/   maintained experiment definitions and publication methodology
tools/        internal data-preparation, validation and summarization utilities
scripts/      small set of maintained user-facing entrypoints
tools/legacy-scripts/ historical campaign wrappers retained only for reproducibility
docs/         architecture, implementation summary and phase history
research/     quarantined/archived research experiments
```

Start with `docs/implementation-summary.md` for the current end-to-end design.
