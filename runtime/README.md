# Runtime

The runtime contains the CosmWasm execution substrate used by ACG and the controlled benchmark harness. The paper artifact executes real Wasm through the common Wasmd evaluator; runnable experiments are under `evaluation/experiments/`.

Core responsibilities are dependency-ready speculative execution, block-local MVCC visibility, receipt/delta capture, concrete read/range/write tracking, indexed reconciliation, selective replay, and adaptive feedback handoff to the Rust scheduler.
