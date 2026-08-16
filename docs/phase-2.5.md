# Phase 2.5: MiniWarehouse structured integration

Phase 2.5 uses MiniWarehouse to stress the concrete transaction graph with variable-size inputs,
compound keys, cross-warehouse stock access, prefix scans, and a deterministic workload source.
It does not add adaptive probabilities or scheduling policy; those begin in Phase 3 and Phase 4.

## Scope

Phase 2.5 adds four things:

1. MiniWarehouse-specific candidate-graph regression tests.
2. Input-resolvable `lines is non-empty` guard evaluation.
3. Input-derived order-line prefix matching for NewOrder, Delivery, and OrderStatus.
4. `acg-miniwarehouse-workload`, a deterministic benchmark workload generator that emits concrete
   runtime `ExecutionRequest` values and plugs into `RateControlledIngress`.

## Variable-size line bindings

The existing Phase 2 predicate evaluator already understands synchronized symbolic indexes. For
this analyzer expression:

```text
(lines[i].supply_warehouse_id, lines[i].item_id)
```

a concrete NewOrder with three lines expands to three tuple keys. Comparison is set based, so a
Restock conflicts when its `(warehouse_id, item_id)` equals any generated line tuple.

This is tested for both local and remote supply warehouses.

## Non-empty guard

MiniWarehouse rejects empty orders before any state access. The analyzer records:

```text
lines is non-empty
```

as an input-only guard on the initial NewOrder accesses. Phase 2.5 compiles this expression to a
small collection-emptiness guard. An empty `lines` array can therefore prune a profile edge before
execution rather than producing an unsupported/unknown guard.

## Order-line prefix refinement

The contract stores concrete line keys as:

```text
(warehouse_id, district_id, order_id, line_number)
```

but Delivery and OrderStatus scan every line under:

```text
(warehouse_id, district_id, order_id)
```

The previous analyzer artifact left those scans unresolved. Phase 2.5 keeps the analyzer JSON
schema unchanged and records the input-derived order prefix as the conflict key for all
`ORDER_LINES` accesses:

```text
(warehouse_id, district_id, order_id)
```

This is sound for this contract:

- NewOrder always numbers non-empty order lines from zero, so two NewOrders with the same prefix
  overlap at line zero.
- Delivery and OrderStatus access the entire prefix.
- Different prefixes are physically disjoint.

The graph still keeps Delivery customer accesses unknown because the customer ID is loaded from the
stored Order record. As a result, MiniWarehouse still has mixed precise/unknown edges, which is a
useful Phase 2.1 regression case.

The updated static graph is:

```text
profiles: 14
edges: 44
conditional_edges: 38
unconditional_edges: 0
unknown_edges: 6
```

## Workload generator

`runtime/crates/acg-miniwarehouse-workload` emits the same JSON execute format as the benchmark
contract without linking the contract crate into the runtime workspace.

Important configuration fields include:

```rust
MiniWarehouseWorkloadConfig {
    scale,
    mix,
    seed,
    initial_stock_quantity,
    min_order_lines,
    max_order_lines,
    max_order_line_quantity,
    unit_price,
    remote_stock_probability_bps,
    hot_warehouse_id,
    hot_warehouse_probability_bps,
    ..
}
```

### Bootstrap

`bootstrap()` emits admin transactions for every configured:

```text
warehouse
district
customer
stock item
```

Districts are initialized with the same first order ID used by the steady-state generator.

### Steady-state traffic

The default operation mix is deliberately TPC-C-inspired rather than benchmark-compliant:

```text
NewOrder  45
Payment   43
Delivery   4
Restock    8
```

Read-only MiniWarehouse queries are tested at the candidate-graph layer but are not inserted into
the transaction mempool.

The generator maintains per-district order IDs and a FIFO queue of generated-but-undelivered
orders. If Delivery is selected before an order exists, it emits a NewOrder instead, preventing an
invalid delivery from being generated solely because of the random mix.

The generator models transaction structure and ordering, not the full committed contract state. For long
runs, provision sufficient `initial_stock_quantity` and/or a Restock share; a NewOrder that fails at
execution time can otherwise make a later generated Delivery invalid. Phase 3 will provide the runtime
feedback needed to close that loop.

### Remote stock

`remote_stock_probability_bps` controls whether each NewOrder line is supplied by the home
warehouse or another configured warehouse. Setting it to `0` forces all-local orders; `10_000`
forces every line remote when more than one warehouse exists.

`hot_warehouse_probability_bps` similarly allows later experiments to create a controlled hot
partition.

## Tests

### Core candidate graph

```bash
cargo test -p acg-candidate-graph --test miniwarehouse -- --nocapture
```

Covers:

- variable-length `lines[i]` tuple expansion;
- exact local and remote stock matching;
- empty-order input guard pruning;
- `StockLevel.item_ids[i]` vector matching;
- warehouse-level Payment contention;
- shared remote stock across otherwise disjoint NewOrders;
- clause-level diagnostics for Delivery versus OrderStatus.

### Symbolic/profile graph

```bash
cargo test -p acg-symbolic-json --test benchmarks -- --nocapture
cargo test -p acg-profile-graph --test benchmarks -- --nocapture
```

Covers the input-derived ORDER_LINES prefix and the remaining state-derived Delivery customer key.

### Runtime adapter

```bash
cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-cosmwasm-adapter \
  --test miniwarehouse \
  -- --nocapture
```

Covers all eight MiniWarehouse execute variants, nested NewOrder line preservation, and concrete
remote-stock edge materialization from a ProducedBlock.

### Workload generator

```bash
cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-miniwarehouse-workload \
  --all-targets \
  -- --nocapture
```

Covers deterministic generation, bootstrap coverage, per-district monotonic order IDs, forced
local/remote supply, Delivery targeting a previously generated order, invalid configuration,
rate-controlled ingress plus FIFO block production, and generator → runtime adapter → candidate-graph
stock matching.

## Deliberately deferred

- runtime read/write feedback and Beta updates;
- state lookups to resolve Delivery's stored customer ID before execution;
- read-only query admission into blocks;
- graph-index performance hardening;
- adaptive scheduling and parallel execution.

The next implementation phase is Phase 3: translate concrete execution/validation evidence into
batched adaptive profile-edge observations.
