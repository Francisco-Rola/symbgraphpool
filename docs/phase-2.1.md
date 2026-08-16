# Phase 2.1: Clause-level conflict resolution

Phase 2.1 refines persistent profile edges so uncertainty belongs to the individual conflict
clause that caused it instead of being represented only by one coarse edge-level relation.

## Motivation

A profile edge is an OR of alternative reasons two entrypoints may conflict. For ConflictLab,
`execute::CreateOrder` and `execute::CancelOrder` can overlap through both `ORDERS` and `BALANCES`:

```text
ORDERS clause
  CreateOrder.order_id == CancelOrder.order_id
  -> input-resolvable

BALANCES clause
  CreateOrder.owner == ORDERS[CancelOrder.order_id].owner
  -> state-derived key, unresolved before execution
```

The edge remains summarized as `unknown`, because at least one alternative clause is unresolved,
but the artifact now preserves that the `ORDERS` clauses are conditional while the `BALANCES`
clauses are unknown. Online predicate evaluation always evaluates the alternatives independently.
It never treats `edge.relation == unknown` as an instruction to materialize an unknown transaction
edge immediately.

## Persisted model

Each `PredicateClause` now contains:

```rust
resolution: ClauseResolution
unknown_reasons: Vec<UnknownReason>
```

`ClauseResolution` is one of:

- `conditional`: instance identity and/or concrete input equality can decide the structural overlap;
- `unconditional`: the clause applies to every concrete pair represented by the two profiles;
- `unknown`: the structural key/scope cannot be fully resolved from the analyzer artifact.

Static unknown reasons currently distinguish:

- `state_derived_key`;
- `unresolved_key`;
- `unknown_scope`.

The same `UnknownReason` type is also used by detailed runtime predicate diagnostics for:

- `missing_input_binding`;
- `unsupported_expression`;
- `state_dependent_guard`;
- `unsupported_guard`.

The coarse `ProfileEdge.relation` is retained as summary metadata and is recomputed from the clause
resolutions:

```text
any unconditional clause                 -> unconditional edge summary
else any unknown clause                  -> unknown edge summary
else                                      -> conditional edge summary
```

Graph loading validates that the stored relation matches this summary and that unknown clauses have
at least one reason.

## Three-valued OR

Each clause evaluates independently to `true`, `false`, or `unknown`. The predicate combines them
with three-valued OR:

```text
true  OR anything -> true
false OR false    -> false
false OR unknown  -> unknown
unknown OR unknown -> unknown
```

This means a precise clause can still prove a conflict even when a different alternative is
unresolved. Conversely, a false precise clause cannot prove independence while an unresolved
alternative remains.

The hot candidate-graph path uses the allocation-free `CompiledPredicate::evaluate`. Debugging and
research instrumentation can use `evaluate_detailed`, which returns one result and unknown-reason
set per clause.

## Artifact version

The portable profile-graph artifact format is now version **2**. Recompile existing analyzer JSON
with `acg-profilec compile`; version-1 graph artifacts are deliberately rejected with an explicit
version error.

## Acceptance tests

Focused tests cover:

- `true OR unknown -> true`;
- `false OR unknown -> unknown`;
- `false OR false -> false`;
- static state-derived/unresolved-key reasons;
- dynamic state-dependent-guard, missing-binding, and unsupported-expression reasons;
- ConflictLab `CreateOrder <-> CancelOrder` preserving both precise `ORDERS` clauses and unknown
  state-derived `BALANCES` clauses;
- candidate-graph materialization using clause results even when the coarse edge relation is
  `unknown`;
- graph-loader rejection of inconsistent relation metadata and malformed unknown clauses;
- clean rejection of legacy version-1 artifacts.

Run:

```bash
cargo test -p acg-predicate --test predicate -- --nocapture
cargo test -p acg-profile-graph --test benchmarks -- --nocapture
cargo test -p acg-profile-graph --test graph -- --nocapture
cargo test -p acg-candidate-graph --test candidate_graph -- --nocapture
```
