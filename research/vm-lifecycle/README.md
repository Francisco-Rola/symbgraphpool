# VM lifecycle research archive

This directory contains the VM-acquisition/lifecycle experiments performed while diagnosing Brick 5C.7 execution efficiency.

**Nothing under `research/vm-lifecycle/` is part of the production runtime or a supported engine configuration.** The production engine uses the canonical CosmWasm fresh-instance path for every Wasm invocation.

## Why this archive exists

The READY-DAG + block-local MVCC executor was functionally correct, but fine-grained CosmWasm transactions showed large concurrent service-cost inflation. We tested a sequence of increasingly aggressive VM-lifecycle ideas to determine where the cost came from without silently turning research behavior into production behavior.

All controlled experiments were eventually capped to a six-physical-core / six-total-thread budget.

## Experiments retained here

1. **Fresh one-use instance pool** — prepare pristine instances ahead of demand and rebind the transaction backends before one-time use.
2. **Deeper reserve / burst-buffer pool** — test whether shallow reserves, rather than production throughput, caused pool waits.
3. **Adaptive preparation** — allow at most one execution worker to temporarily prepare instances rather than permanently reserving a core.
4. **Acquisition throttling / perf diagnostics** — measure whether limiting concurrent instantiation improved contention.
5. **Unsafe retained dirty instances** — deliberately reuse an already-executed VM as a performance upper bound. This is semantically invalid and remains a negative correctness control.
6. **Fresh cache shards** — use independent prewarmed CosmWasm caches while still constructing a genuinely fresh Store/Instance for each transaction.
7. **Compute, READY-DAG-shape, and storage/MVCC probes** — separate scheduler realization, transaction granularity, host storage, and VM lifecycle effects.

## Current research conclusion

The experiments ruled out READY-DAG scheduling and ordinary conflict-free point MVCC as the primary performance problem. The dependency executor generally realizes the observed-cost DAG within a few percent, and compute-heavy independent transactions reach roughly 5.2x wall-clock speedup on a six-core machine.

The lifecycle experiments showed:

- unsafe dirty retention is much faster, but leaks VM-local state and therefore cannot be used;
- a fresh-instance pool can improve MiniWarehouse under some reserve configurations, but producer wait, background CPU use, and memory/cache interference make it workload-sensitive;
- adaptive preparation is preferable to permanently reserving one of six cores, but still steals execution capacity when the READY frontier can saturate all cores;
- independent fresh cache shards reduce some acquisition cost but recover only a small/inconsistent fraction of the dirty-retained upper bound;
- therefore the remaining high-value VM question is **the cost of constructing and destroying genuinely fresh mutable Wasmer/CosmWasm state**, not merely shared cache lookup/locking.

A future research branch may investigate a correct pristine snapshot / copy-on-write / fresh-mutable-state reconstruction primitive below the public `cosmwasm_vm::Instance` abstraction. That work is intentionally not present in the production engine yet.

## Directory contents

- `archive.zip` — compressed research bundle containing all incremental patches, raw summaries, experimental runners, research-only Wasm probes, and the selected final experimental source snapshot.
- `ARCHIVE_MANIFEST.txt` — exact file list inside `archive.zip`.

The archive is intentionally opaque to the production Cargo workspaces: nothing inside it is compiled or linked unless explicitly extracted onto a separate research branch.

## Reproduction policy

Do not copy individual research source files into production. Start from a clean branch, apply the relevant archived patch series in order, and run the associated correctness gates before performance experiments. Dirty retained-instance reuse must never be interpreted as a correctness-preserving optimization.
