#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

START_BLOCK=16774645
END_BLOCK=16779644
OUT="${VEGETA_S1_EXACT_DIR:-benchmarks/corpora/vegeta-ethereum/s1-exact-sload-sstore}"
TRACE_MODE="${VEGETA_S1_EXACT_TRACE_MODE:-custom-js}"
TIMEOUT="${VEGETA_S1_EXACT_RPC_TIMEOUT:-900}"
RETRIES="${VEGETA_S1_EXACT_RPC_RETRIES:-8}"
BACKOFF="${VEGETA_S1_EXACT_RPC_BACKOFF:-1.5}"
TX_DELAY="${VEGETA_S1_EXACT_TX_DELAY:-0}"
PROBE_ONLY="${VEGETA_S1_EXACT_PROBE_ONLY:-0}"
FALLBACK_TXS="${VEGETA_S1_EXACT_FALLBACK_TXS:-}"
FALLBACK_FILE="${VEGETA_S1_EXACT_FALLBACK_FILE:-evaluation/vegeta/s1-exact-trace-fallbacks.v1.txt}"
FALLBACK_CORPUS="${VEGETA_S1_FALLBACK_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"

if [[ -z "${ETH_RPC_URL:-}" ]]; then
  echo "set ETH_RPC_URL to an archive-capable Ethereum endpoint with custom tracer support" >&2
  exit 2
fi

case "$TRACE_MODE" in
  custom-js|custom-js-tx) ;;
  *)
    echo "VEGETA_S1_EXACT_TRACE_MODE must be custom-js or custom-js-tx (got $TRACE_MODE)" >&2
    exit 2
    ;;
esac

mkdir -p "$OUT"

EXTRA_ARGS=()
FALLBACK_HASH_ARRAY=()
declare -A SEEN_FALLBACK_TXS=()

add_fallback_tx() {
  local tx_hash="$1"
  tx_hash="${tx_hash//[[:space:]]/}"
  tx_hash="${tx_hash,,}"
  [[ -z "$tx_hash" ]] && return 0
  [[ "$tx_hash" == \#* ]] && return 0
  if [[ -z "${SEEN_FALLBACK_TXS[$tx_hash]+x}" ]]; then
    SEEN_FALLBACK_TXS["$tx_hash"]=1
    FALLBACK_HASH_ARRAY+=("$tx_hash")
  fi
}

if [[ -n "$FALLBACK_FILE" && -f "$FALLBACK_FILE" ]]; then
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%#*}"
    add_fallback_tx "$line"
  done < "$FALLBACK_FILE"
fi

if [[ -n "$FALLBACK_TXS" ]]; then
  IFS=',' read -r -a ENV_FALLBACK_HASH_ARRAY <<< "$FALLBACK_TXS"
  for tx_hash in "${ENV_FALLBACK_HASH_ARRAY[@]}"; do
    add_fallback_tx "$tx_hash"
  done
fi

if (( ${#FALLBACK_HASH_ARRAY[@]} > 0 )); then
  if [[ "$TRACE_MODE" != "custom-js-tx" ]]; then
    echo "explicit fallback transactions require VEGETA_S1_EXACT_TRACE_MODE=custom-js-tx" >&2
    exit 2
  fi
  if [[ ! -f "$FALLBACK_CORPUS" ]]; then
    echo "fallback corpus not found: $FALLBACK_CORPUS" >&2
    exit 2
  fi
  EXTRA_ARGS+=(--fallback-corpus "$FALLBACK_CORPUS")
  for tx_hash in "${FALLBACK_HASH_ARRAY[@]}"; do
    EXTRA_ARGS+=(--fallback-tx-hash "$tx_hash")
  done
fi

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
ARGS+=("${EXTRA_ARGS[@]}")

if [[ "$PROBE_ONLY" == "1" ]]; then
  ARGS+=(--probe-only)
fi

python3 tools/vegeta/extract-vegeta-ethereum.py "${ARGS[@]}"

if [[ "$PROBE_ONLY" == "1" ]]; then
  echo
  echo "PASS: exact S1 tracer probe completed at block $START_BLOCK using $TRACE_MODE"
  exit 0
fi

python3 tools/vegeta/validate-vegeta-corpus.py \
  "$OUT/corpus.jsonl" \
  --dataset-tag S1 \
  --json-output "$OUT/validation-report.json"

echo
echo "PASS: exact SLOAD/SSTORE Vegeta S1 corpus reconstructed"
echo "range: $START_BLOCK..$END_BLOCK (5000 blocks)"
echo "trace mode: $TRACE_MODE"
echo "corpus: $OUT/corpus.jsonl"
echo "manifest: $OUT/manifest.json"
echo "validation: $OUT/validation-report.json"
if [[ "$TRACE_MODE" == "custom-js-tx" ]]; then
  echo "transaction checkpoints: $OUT/tx-traces/"
fi
if (( ${#FALLBACK_HASH_ARRAY[@]} > 0 )); then
  echo "WARNING: ${#FALLBACK_HASH_ARRAY[@]} explicit fallback transaction(s) weaken exact-trace semantics; inspect manifest.trace_semantics_exceptions"
fi
