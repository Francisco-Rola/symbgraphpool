# Next brick: concrete transaction edge materialization

Brick 1 stops at the persistent profile graph boundary. The next implementation should add:

1. `InstanceId` and a runtime-neutral candidate transaction envelope.
2. Compile normalized delegation-frame mappings into compact binding IDs rather than strings.
3. Compilation of `PredicateTemplate` clauses into bytecode or specialized evaluators.
4. Transaction bucketing by `ProfileId` and `InstanceId`.
5. Candidate edge materialization from profile adjacency only.
6. A binary greedy wave scheduler before adaptive probabilities are introduced.

The first predicate compiler only needs these operations:

```text
SAME_INSTANCE
LOAD_LEFT_INPUT binding_id
LOAD_RIGHT_INPUT binding_id
EQUAL
EVAL_LEFT_GUARD predicate_id
EVAL_RIGHT_GUARD predicate_id
AND / OR
UNKNOWN
```

`KeyMatch::Unresolved` should return an explicit three-valued result, not `false`, so the scheduler
can apply a conservative fallback probability.
