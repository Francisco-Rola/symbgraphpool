#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# Vegeta NSDI'25 Table 2, dataset S4.
START_BLOCK=18581726
END_BLOCK=18586725
OUT="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
TIMEOUT="${VEGETA_S4_RPC_TIMEOUT:-600}"
RETRIES="${VEGETA_S4_RPC_RETRIES:-8}"
BACKOFF="${VEGETA_S4_RPC_BACKOFF:-1.5}"
PROBE_ONLY="${VEGETA_S4_PROBE_ONLY:-0}"

if [[ -z "${ETH_RPC_URL:-}" ]]; then
  echo "set ETH_RPC_URL to an archive-capable Ethereum mainnet endpoint" >&2
  exit 2
fi

mkdir -p "$OUT"

ARGS=(
  --trace-mode public-rpc
  --start-block "$START_BLOCK"
  --end-block "$END_BLOCK"
  --output-dir "$OUT"
  --timeout "$TIMEOUT"
  --rpc-retries "$RETRIES"
  --retry-backoff "$BACKOFF"
  --resume
)

if [[ "$PROBE_ONLY" == "1" ]]; then
  ARGS+=(--probe-only)
fi

python3 tools/vegeta/extract-vegeta-ethereum.py "${ARGS[@]}"

if [[ "$PROBE_ONLY" == "1" ]]; then
  echo
  echo "PASS: S4 public-RPC probe completed at block $START_BLOCK"
  exit 0
fi

python3 tools/vegeta/validate-vegeta-corpus.py \
  "$OUT/corpus.jsonl" \
  --dataset-tag S4 \
  --json-output "$OUT/validation-report.json"

echo
echo "PASS: Vegeta S4 portable corpus reconstructed"
echo "range: $START_BLOCK..$END_BLOCK (5000 blocks)"
echo "trace mode: public-rpc (prestateTracer touched storage + diff-mode changed storage)"
echo "corpus: $OUT/corpus.jsonl"
echo "manifest: $OUT/manifest.json"
echo "validation: $OUT/validation-report.json"
echo
echo "Paper provenance targets: 747651 tx, longest-chain sum 89961, ratio 8.31."
echo "No exact custom-JS SLOAD/SSTORE collection is performed by this script."
