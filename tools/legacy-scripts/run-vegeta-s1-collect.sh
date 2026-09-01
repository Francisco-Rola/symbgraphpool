#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

START_BLOCK=16774645
END_BLOCK=16779644
OUT="${VEGETA_S1_DIR:-benchmarks/corpora/vegeta-ethereum/s1}"
TRACE_MODE="${VEGETA_S1_TRACE_MODE:-public-rpc}"
TIMEOUT="${VEGETA_S1_RPC_TIMEOUT:-600}"
RETRIES="${VEGETA_S1_RPC_RETRIES:-8}"
BACKOFF="${VEGETA_S1_RPC_BACKOFF:-1.5}"
TX_DELAY="${VEGETA_S1_TX_DELAY:-0}"
PROBE_ONLY="${VEGETA_S1_PROBE_ONLY:-0}"

if [[ -z "${ETH_RPC_URL:-}" ]]; then
  echo "set ETH_RPC_URL to an archive-capable Ethereum mainnet endpoint" >&2
  exit 2
fi

case "$TRACE_MODE" in
  public-rpc|custom-js|custom-js-tx) ;;
  *)
    echo "VEGETA_S1_TRACE_MODE must be public-rpc, custom-js, or custom-js-tx (got $TRACE_MODE)" >&2
    exit 2
    ;;
esac

mkdir -p "$OUT"

ARGS=(
  --trace-mode "$TRACE_MODE"
  --start-block "$START_BLOCK"
  --end-block "$END_BLOCK"
  --output-dir "$OUT"
  --timeout "$TIMEOUT"
  --rpc-retries "$RETRIES"
  --retry-backoff "$BACKOFF"
  --tx-delay "$TX_DELAY"
  --resume
)

if [[ "$PROBE_ONLY" == "1" ]]; then
  ARGS+=(--probe-only)
fi

python3 tools/vegeta/extract-vegeta-ethereum.py "${ARGS[@]}"

if [[ "$PROBE_ONLY" == "1" ]]; then
  echo
  echo "PASS: S1 RPC probe completed at block $START_BLOCK using $TRACE_MODE"
  exit 0
fi

python3 tools/vegeta/validate-vegeta-corpus.py \
  "$OUT/corpus.jsonl" \
  --dataset-tag S1 \
  --json-output "$OUT/validation-report.json"

echo
echo "PASS: Vegeta S1 corpus reconstructed"
echo "range: $START_BLOCK..$END_BLOCK (5000 blocks)"
echo "trace mode: $TRACE_MODE"
echo "corpus: $OUT/corpus.jsonl"
echo "manifest: $OUT/manifest.json"
echo "validation: $OUT/validation-report.json"
echo
echo "Paper targets are retained as provenance: 739863 tx, longest-chain sum 88136, ratio 8.39."
echo "Only block/range/ordering invariants are mandatory unless stricter validator flags are requested."
