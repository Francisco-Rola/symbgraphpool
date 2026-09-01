# Vegeta Ethereum S1 corpus

Vegeta NSDI'25 Table 2 defines **S1** as Ethereum mainnet blocks **16,774,645 through
16,779,644 inclusive**: 5,000 blocks. The paper reports 739,863 transactions, a summed
longest-chain metric of 88,136, and ratio 8.39. S1 is the paper's principal single-node
performance and correctness workload.

Ethereum history is not vendored in this repository. Reconstruct it from an archive-capable RPC.

## 1. Probe the provider

Hosted-provider-compatible built-in tracing:

```bash
ETH_RPC_URL=https://YOUR_RPC \
VEGETA_S1_PROBE_ONLY=1 \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

For a self-hosted Geth node that permits arbitrary JavaScript tracers:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
VEGETA_S1_TRACE_MODE=custom-js \
VEGETA_S1_PROBE_ONLY=1 \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

## 2. Collect the portable corpus

```bash
ETH_RPC_URL=https://YOUR_RPC \
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
```

The default `public-rpc` mode uses Geth's built-in `prestateTracer` in normal and diff modes.
As with S3, it records conservative touched-storage reads and state-changing writes. All block files
are atomic and `--resume` is always enabled, so interrupting and rerunning the command is safe.

Outputs:

- `blocks/<height>.json`: one durable block checkpoint per Ethereum block;
- `corpus.jsonl`: ordered 5,000-block corpus assembled after collection finishes;
- `manifest.json`: trace semantics, exact paper range, paper targets, and observed metrics;
- `validation-report.json`: shape and provenance validation.

## 3. Collect exact SLOAD/SSTORE accesses

For publication/oracle work, prefer exact custom-JS tracing. A self-hosted archive Geth is the
practical fast path because `custom-js` traces once per block:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
bash tools/legacy-scripts/run-vegeta-s1-exact-trace.sh
```

If a hosted provider allows arbitrary custom tracers only through `debug_traceTransaction`, use the
checkpointed transaction mode instead:

```bash
ETH_RPC_URL=https://YOUR_RPC \
VEGETA_S1_EXACT_TRACE_MODE=custom-js-tx \
VEGETA_S1_EXACT_TX_DELAY=0.02 \
bash tools/legacy-scripts/run-vegeta-s1-exact-trace.sh
```

That mode checkpoints every transaction under `s1-exact-sload-sstore/tx-traces/`, so a collection
that takes many hours or days can be stopped and resumed without losing completed traces.

The exact collector intentionally fails closed. If a provider cannot trace a particular transaction,
do not silently drop it. The optional `VEGETA_S1_EXACT_FALLBACK_TXS` / `VEGETA_S1_EXACT_FALLBACK_FILE`
mechanism exists only for audited exceptions and records those exceptions in the manifest.

## Validation policy

The block range and transaction ordering are corpus identity. The paper's aggregate transaction
count, longest-chain value, ratio, and WETH hotspot are provenance/instrumentation targets rather
than silently assumed ground truth. Once an independent canonical S1 transaction-count audit is
frozen, it can be promoted to a hard identity check in `tools/vegeta/vegeta_corpus.py`.

## Native translation fidelity gates

After the RPC corpus/callTracer/code caches are frozen, S1 native preparation uses a separate reviewed
state-semantics audit. The source `prestateTracer` corpus measures conservative storage touches, not
only committed writes. Consequently a reviewed state query is a real dependency (`STATE_READ`), while
a pure computation is not; reviewed reverted state paths are retained only in the denominator-aligned
state-touch view and excluded from the successful/committed-path diagnostic.

Run the local-only audit with:

```bash
bash tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh
```

Full native preparation refuses publication mode unless reviewed state-touch conflict coverage is at
least 95%, median conflict-bearing-block coverage is at least 80%, and successful reviewed-state
transaction coverage is at least 80% (unless an explicit diagnostic-only low-coverage override is set).
Concrete source access sets remain offline audit inputs and are never copied into `native-plan.jsonl`.

