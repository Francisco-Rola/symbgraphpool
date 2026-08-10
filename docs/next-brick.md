# Next brick: runtime observations and adaptive profile-edge statistics

Brick 2.5 now drives structured MiniWarehouse transactions through concrete input binding,
candidate-graph construction, and the validator ingress/block path. The next work is Brick 3.

## Brick 3: runtime observations

1. Translate execution `AccessRecord`s into normalized concrete resource accesses.
2. Associate observed overlaps with persistent profile edges and concrete predicate context.
3. Distinguish positive conflict evidence, compared-and-independent evidence, and pairs that were
   never compared.
4. Buffer observations per block and aggregate updates outside the critical execution loop.
5. Implement symbolic Beta priors, weighted pre/post observations, posterior mean, and confidence.
6. Add epoch-based exponential decay.
7. Persist adaptive alpha/beta/timestamp arrays separately from immutable graph topology.
8. Create runtime-discovered fallback edges when observed accesses reveal a profile relationship
   missing from the symbolic graph.
9. Add ConflictLab and MiniWarehouse tests for repeated positive/negative observations and workload
   shifts.

Weighted scheduling and speculative parallel execution remain later bricks. The serial executor
continues to be the correctness baseline while runtime learning is introduced.
