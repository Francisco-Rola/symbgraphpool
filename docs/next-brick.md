# Next brick: weighted candidate graph and adaptive scheduling

Brick 3 now translates concrete execution/validation evidence into decayed Beta statistics and
persistent runtime-discovered fallback edges. The next work is Brick 4.

## Brick 4: weighted scheduling

### Brick 4A policy: runtime evidence can override symbolic pruning

Concrete execution is authoritative evidence for scheduling belief, while symbolic predicates remain
advisory. There are two miss classes:

- **topology miss:** no static profile edge exists; Brick 3 creates a persistent runtime-discovered
  fallback edge;
- **predicate/materialization miss:** a static profile edge exists, but the concrete candidate pair was
  pruned and execution later proves a conflict.

Brick 4A persists `candidate_miss_observations` on the affected adaptive statistics. Brick 4B will use
that history as the gate for bypassing an otherwise-false symbolic predicate. A symbolic prior or
unrelated positive history alone must not turn every false concrete predicate into an edge. Once a
real candidate miss has been observed, however, the symbolic predicate is no longer treated as an
absolute proof of independence; the learned posterior may materialize the relationship subject to the
weighted graph's normal probability threshold.

Brick 4A also exposes non-mutating current-epoch estimates for static and runtime-discovered edges
and maintains profile adjacency for fallback edges so candidate construction can traverse learned
topology without a global fallback-edge scan.

1. Expose current-epoch probability/confidence estimates for static and fallback profile edges.
2. Materialize fallback profile relationships into the candidate transaction graph.
3. Attach compact conflict probabilities to concrete transaction edges.
4. Define configurable hard and soft thresholds.
5. Orient hard dependencies by predicted block order.
6. Implement the risk-bounded wave scheduler using cumulative soft-edge risk.
7. Keep FIFO/serial scheduling as a baseline implementation.
8. Add deterministic ConflictLab tests for threshold boundaries and wave placement.
9. Add MiniWarehouse tests for local/remote stock contention and workload skew.
10. Add scheduler metrics for edge classes, wave width, estimated risk, and construction latency.

Speculative parallel commit remains disabled until Brick 5 introduces isolated execution,
canonical validation, invalidation, and selective replay. Brick 4 can therefore validate scheduling
logic while the serial executor remains the correctness baseline.
