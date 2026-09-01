# Vegeta S1 exact SLOAD/SSTORE corpus

This directory is populated by:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
bash tools/legacy-scripts/run-vegeta-s1-exact-trace.sh
```

It uses the repository's custom Geth tracer to record exact EVM `SLOAD` and `SSTORE` accesses for
Vegeta S1 (blocks 16,774,645..16,779,644). The generated history is intentionally git-ignored; this
README documents how to reconstruct it.

Use `VEGETA_S1_EXACT_TRACE_MODE=custom-js-tx` for transaction-level checkpointing when block-level
custom tracing is unavailable or unreliable through the chosen RPC provider.
