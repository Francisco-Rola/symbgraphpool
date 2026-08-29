# Wasmd single-cache / lock-free tracker optimization

This change targets the measured integration overhead in the Wasmd SymbGraph
and Vegeta ports.

- Normal speculative attempts now execute directly on their existing private
  CacheMultiStore branch (`executeTxIsolated`) instead of adding a second
  top-level `CacheContext`.
- Expected source-failure transactions keep the old inner cache because their
  semantics require discarding every top-level write.
- Reverted source scopes still execute in disposable child caches so their
  reads remain visible to validation while their writes are discarded.
- Actual-access tracking is lock-free under a documented single-owner
  transaction-branch invariant.
- Wasmd SymbGraph/Vegeta rows now export real speculation/reuse counters.
- Investigation mode adds `tracked-single-cache-serial`, a serial control for
  the optimized isolation/tracker substrate.

Run `scripts/run-vegeta-s3-wasmd-optimized-w2.sh` first. If state equivalence
passes and the optimized control behaves as expected, use
`scripts/run-vegeta-s3-wasmd-optimized-sweep.sh`.
