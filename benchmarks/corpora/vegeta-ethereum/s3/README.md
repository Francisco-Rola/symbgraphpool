# Vegeta Ethereum S3 corpus

This directory intentionally does **not** vendor Ethereum history. Reconstruct the public workload
from an archive-capable Ethereum endpoint that exposes `debug_traceBlockByNumber` with the built-in
`prestateTracer`.

Hosted/public-provider-compatible mode is now the default. Probe one S3 block before launching the
full 101-block extraction:

```bash
ETH_RPC_URL=https://YOUR_ETHEREUM_RPC \
python3 scripts/vegeta/extract-vegeta-ethereum.py \
  --trace-mode public-rpc \
  --start-block 16774645 \
  --output-dir benchmarks/corpora/vegeta-ethereum/s3 \
  --probe-only
```

Then reconstruct S3:

```bash
ETH_RPC_URL=https://YOUR_ETHEREUM_RPC \
python3 scripts/vegeta/extract-vegeta-ethereum.py \
  --trace-mode public-rpc \
  --output-dir benchmarks/corpora/vegeta-ethereum/s3 \
  --resume

python3 scripts/vegeta/validate-vegeta-corpus.py \
  benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl
```

`public-rpc` makes only built-in tracer calls. It runs `prestateTracer` once in normal mode to obtain
touched storage leaves and once with `diffMode=true` to identify state-changing storage slots. It
also fetches transaction receipts for gas-used/failure metadata. If `eth_getBlockReceipts` is not
available, the extractor automatically falls back to `eth_getTransactionReceipt` per transaction.
Transient HTTP/RPC failures are retried with exponential backoff.

Because the built-in prestate tracer does not distinguish individual SLOAD and SSTORE opcodes, this
mode deliberately uses conservative semantics:

- `reads`: every storage slot touched by the transaction;
- `writes`: storage slots whose state changed according to `diffMode`;
- `opcode_steps`: gas used, retained as the deterministic compute-cost proxy for the replay VM.

The generated manifest records these semantics. They are **not** silently treated as identical to
the exact custom-JS trace.

For a self-hosted Geth node that allows arbitrary JavaScript tracers, the original higher-fidelity
mode remains available:

```bash
ETH_RPC_URL=http://127.0.0.1:8545 \
python3 scripts/vegeta/extract-vegeta-ethereum.py \
  --trace-mode custom-js \
  --output-dir benchmarks/corpora/vegeta-ethereum/s3 \
  --resume
```

The NSDI'25 Vegeta paper defines S3 as Ethereum blocks 16,774,645 through 16,774,745 (101 blocks,
15,129 transactions), with a summed longest-chain metric of 1,779 and ratio 8.50. The public paper
and repository do not publish their exact internal read/write-set corpus.

Block boundaries, transaction order, transaction hashes, and transaction count are exact validation
targets. The paper's longest-chain number and WETH-hotspot result are reported as
instrumentation-equivalence diagnostics. Use `--require-paper-chain-match --require-weth-hotspot`
only after confirming the selected trace semantics reproduce those unpublished internals.

The generated files are ignored by git; keep `README.md` under version control.
