# Single-validator runtime simulation

## Purpose

The validator simulator supplies the timing and queueing environment needed for later candidate
transaction graph and scheduling experiments. It is not a consensus implementation and does not
attempt to model peer-to-peer propagation, proposer election, voting, forks, or finality.

## Data flow

```text
workload generator
    -> RateControlledIngress
    -> all-accepting FIFO Mempool
    -> BlockProducer<BlockSelectionPolicy>
    -> BlockScheduler
    -> ExecutionPlan { waves }
    -> BlockExecutor
    -> CosmWasmEngine
```

All timing uses virtual nanoseconds. A benchmark can advance directly to a block boundary without
waiting for wall-clock time. Ingress places its first arrival after one inter-arrival interval, so a
rate of `r` TPS produces exactly `2r` arrivals by the inclusive default two-second boundary.

## Defaults

- ingress: 25,000 transactions per second;
- block window: 2 seconds, with the first default block timestamp at 2 seconds;
- selection: all currently admitted transactions in FIFO order;
- scheduling: FIFO, one transaction per wave;
- execution: serial;
- failed transactions: state is rolled back by the engine and block execution continues.

The ingress default is a benchmark reference derived from Injective's published throughput figure.
The two-second block window is the project's requested speculation window and is independently
configurable.

## Extension points

`BlockSelectionPolicy` will support future size, gas, fee, account, or benchmark-specific block
construction policies.

`BlockScheduler` will consume the Brick 2 candidate transaction graph and produce hard/soft
conflict-aware waves.

`BlockExecutor` will gain a speculative parallel implementation only after isolated execution,
validation, canonical-order reconciliation, and selective replay are available. The current serial
executor rejects waves wider than one transaction so unsafe concurrency cannot be enabled by
accident.
