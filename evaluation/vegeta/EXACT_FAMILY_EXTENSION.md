# Vegeta S3 exact-ground-truth family extension

This extension follows the exact SLOAD/SSTORE fidelity pass. It does **not** change the previously
frozen 95% aggregate-conflict, 80% median conflict-bearing-block, 75% semantic-transaction, or 50%
semantic-call-frame thresholds.

The exact trace exposed four dominant unmapped storage owners that belong to the already pre-declared
`marketplace-router` candidate family: Seaport 1.4, Uniswap Universal Router V1, Blur Exchange, and
Seaport 1.1. `s3-native-family-map.v2.json` preserves all version-1 mappings and adds those four
reviewed profile families. `s3-exact-family-extension.v1.json` records the owner/profile identifiers,
reviewed selectors, public source references, and the selectors deliberately left opaque. The
semantic extension uses owner-level attribution only to decide which pre-declared family deserves
review; it does not ingest concrete historical storage keys into the planner, executor, or symbolic
profiles.

Marketplace settlement order IDs in the native adapter are deterministic SHA-256 fingerprints of
public calldata. They are semantic input identifiers, not reconstructed EVM storage keys. This may
under-alias the same logical order when it is encoded differently, so residual false negatives remain
an evaluation result rather than something to be tuned away. Universal Router execution/callback
traffic receives a per-instance execution-lock resource, and Blur settlement receives a separate
per-instance internal-execution guard plus order-status state. Unknown observed selectors remain
opaque.

The wrapped-native contract now treats `unative` as immutable code configuration. Instantiate still
checks the manifest-supplied denomination for compatibility, but no `denom` item is persisted or read.
Deposit, withdraw, and total-supply therefore cannot create the prior artificial contract-storage
singleton dependency; native bank-ledger dependencies remain real and separately measured.

The exact follow-up evaluator now requires `native-plan/final-mapping-simulation.json` for semantic
transaction/frame gates. `translation-coverage.json` is retained as provenance and for pre-finalized
coverage diagnostics, but its stale semantic-volume values are no longer eligible for the exact gate
recheck.

Run static/provenance/unit validation with:

```bash
bash tools/legacy-scripts/validate-vegeta-s3-exact-family-extensions.sh
```

After the exact SLOAD/SSTORE corpus already exists, run the full extension evaluation with:

```bash
ETH_RPC_URL=<archive-capable-rpc> \
  bash tools/legacy-scripts/run-vegeta-s3-exact-family-extension-evaluation.sh
```

The evaluation wrapper does not re-extract exact traces. It rebuilds the native plan against the
exact source corpus, reruns finalization and native execution, runs the exact follow-up, and validates
only the previously frozen gates plus mechanical invariants of this patch. It intentionally adds no
new precision/recall/critical-path threshold after seeing the result.
