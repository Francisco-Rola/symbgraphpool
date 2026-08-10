# Brick 5 — Safe speculative execution

Brick 5 turns Brick 4's advisory waves into real speculative execution while preserving canonical
serial semantics through validation and replay.

## Brick 5A — isolated speculative state and receipts

Brick 5A introduces the execution substrate only; it does **not** validate, commit speculative
results, replay transactions, or add worker threads yet.

`CosmWasmEngine::snapshot()` captures a detached transaction-visible copy of world state.
`execute_speculative()` runs an instantiate/execute request against that snapshot and returns a
`SpeculativeTxResult` without mutating canonical state.

A receipt contains:

- the success/failure status and successful output metadata;
- attempted access records;
- correctness-level read dependencies;
- a deterministic commit-ready storage/bank/contract write set for successful transactions.

Read dependencies cover contract metadata, point storage reads, storage ranges, point bank reads,
and all-balances enumeration. Reads satisfied by transaction-local writes are not base-state
dependencies. Range/all-balances dependencies retain enough base information to detect phantoms in
Brick 5B.

Reads performed inside reverted child calls are retained because they can influence a handled
failure/reply and therefore remain relevant to correctness validation. Reverted child writes do
not enter the final write set. A failed top-level transaction has an empty commit-ready write set
and its attempted accesses are marked reverted.

The same snapshot can be shared by future worker threads because transactions use independent
transaction overlays and never commit into the snapshot.

## Remaining Brick 5 stages

- **5C:** bounded parallel worker pool for wave execution.
- **5D:** validation/replay evidence fed into adaptive statistics without double counting.
- **5E:** theoretical versus realized parallelism and execution-efficiency metrics.
- **5F:** ConflictLab/MiniWarehouse serial-equivalence and performance acceptance.

## Brick 5B: canonical validation, reuse, and selective replay

Brick 5B keeps execution single-threaded while proving the correctness state machine that Brick 5C will run beneath a worker pool.

A speculative receipt is now bound to the exact `BlockContext`, `ExecutionRequest`, and engine that produced it. The canonical coordinator drains transactions strictly in block order. For each speculative receipt it validates all 5A read dependencies against the current canonical predecessor state:

- contract metadata/existence;
- point storage reads, including observed absence;
- storage ranges with transaction-local masked keys removed, detecting value changes and insertion/deletion phantoms;
- point bank balances;
- all-balances enumeration with transaction-local masked denominations removed.

A valid successful receipt commits its detached `StateWriteSet` atomically under the canonical world-state write lock and reuses the speculative events/data/result without re-executing the contract. A valid failed receipt reuses the failure and commits no writes. An invalid receipt is discarded and the original transaction is replayed through the canonical execution path.

Blind writes deliberately do not create read dependencies. Therefore a later canonical blind write can reuse its speculative receipt even when an earlier predecessor wrote the same key: canonical ordering still makes the later write authoritative.

Brick 5B also introduces reuse/validation/replay metrics. Actual worker parallelism and wall-clock speedup remain Brick 5C/5E concerns.
