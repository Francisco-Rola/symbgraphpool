#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT="${VEGETA_S3_EXACT_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore}"
TIMEOUT="${VEGETA_S3_EXACT_RPC_TIMEOUT:-90}"
RETRIES="${VEGETA_S3_EXACT_RPC_RETRIES:-5}"
BACKOFF="${VEGETA_S3_EXACT_RPC_BACKOFF:-1.5}"
TX_DELAY="${VEGETA_S3_EXACT_TX_DELAY:-0}"
FALLBACK_TXS="${VEGETA_S3_EXACT_FALLBACK_TXS:-}"
FALLBACK_FILE="${VEGETA_S3_EXACT_FALLBACK_FILE:-evaluation/vegeta/s3-exact-trace-fallbacks.v1.txt}"
FALLBACK_CORPUS="${VEGETA_S3_FALLBACK_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"

if [[ -z "${ETH_RPC_URL:-}" ]]; then
  echo "set ETH_RPC_URL to an archive-capable Ethereum endpoint with debug_traceTransaction custom-tracer support" >&2
  exit 2
fi

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
  EXTRA_ARGS+=(--fallback-corpus "$FALLBACK_CORPUS")
  for tx_hash in "${FALLBACK_HASH_ARRAY[@]}"; do
    EXTRA_ARGS+=(--fallback-tx-hash "$tx_hash")
  done
fi

python3 scripts/vegeta/extract-vegeta-ethereum.py \
  --trace-mode custom-js-tx \
  --output-dir "$OUT" \
  --timeout "$TIMEOUT" \
  --rpc-retries "$RETRIES" \
  --retry-backoff "$BACKOFF" \
  --tx-delay "$TX_DELAY" \
  --resume \
  "${EXTRA_ARGS[@]}"

python3 scripts/vegeta/validate-vegeta-corpus.py \
  "$OUT/corpus.jsonl" \
  --json-output "$OUT/validation-report.json"

echo
echo "PASS: exact transaction-level SLOAD/SSTORE S3 corpus reconstructed"
echo "corpus: $OUT/corpus.jsonl"
echo "manifest: $OUT/manifest.json"
if (( ${#FALLBACK_HASH_ARRAY[@]} > 0 )); then
  echo "explicit fallback tx(s):"
  for tx_hash in "${FALLBACK_HASH_ARRAY[@]}"; do
    echo "  $tx_hash"
  done
  [[ -n "$FALLBACK_FILE" ]] && echo "fallback list: $FALLBACK_FILE"
  echo "fallback corpus: $FALLBACK_CORPUS"
fi
echo
echo "To rerun native topology fidelity against this exact source corpus:"
echo "  VEGETA_S3_CORPUS=$OUT/corpus.jsonl ETH_RPC_URL=\$ETH_RPC_URL bash scripts/run-vegeta-s3-native-execution.sh"
