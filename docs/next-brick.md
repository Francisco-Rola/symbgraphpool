# Next brick: weighted candidate graph and adaptive scheduling

Brick 3 now translates concrete execution/validation evidence into decayed Beta statistics and
persistent runtime-discovered fallback edges. The next work is Brick 4.

## Brick 4: weighted scheduling

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
