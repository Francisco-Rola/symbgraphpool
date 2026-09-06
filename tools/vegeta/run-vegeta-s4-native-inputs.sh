#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S4_CORPUS:-benchmarks/corpora/vegeta-ethereum/s4/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s4/native-characterization}"
RPC="${ETH_RPC_URL:-}"
REUSE_CACHED="${VEGETA_S4_REUSE_CACHED_INPUTS:-0}"

[[ -s "$CORPUS" ]] || {
  echo "missing Vegeta S4 corpus: $CORPUS" >&2
  echo "run tools/vegeta/run-vegeta-s4-collect.sh first" >&2
  exit 2
}

mkdir -p "$WORK_DIR/call-cache"

# This helper is S1-named for historical reasons but is range-agnostic: it streams any
# Vegeta corpus into a thin metadata corpus and a first-seen historical address list.
python3 tools/vegeta/prepare-vegeta-s1-inputs.py \
  --corpus "$CORPUS" \
  --output-dir "$WORK_DIR"

if [[ "$REUSE_CACHED" == "1" ]]; then
  [[ -s "$WORK_DIR/code-cache.json" ]] || {
    echo "VEGETA_S4_REUSE_CACHED_INPUTS=1 but code cache is missing: $WORK_DIR/code-cache.json" >&2
    exit 2
  }
  [[ -d "$WORK_DIR/call-cache" ]] || {
    echo "VEGETA_S4_REUSE_CACHED_INPUTS=1 but call cache is missing: $WORK_DIR/call-cache" >&2
    exit 2
  }
  echo "S4 native input refresh: reusing code/callTracer caches; no RPC calls"
else
  [[ -n "$RPC" ]] || {
    echo "set ETH_RPC_URL to an archive-capable endpoint with debug_traceBlockByNumber callTracer support" >&2
    exit 2
  }

  # Pass 1: direct destinations and storage owners discovered by the public-RPC corpus.
  python3 tools/vegeta/fetch-vegeta-historical-code.py \
    --addresses "$WORK_DIR/relevant-addresses.json" \
    --output "$WORK_DIR/code-cache.json" \
    --rpc-url "$RPC" \
    --rpc-timeout "${VEGETA_S4_CODE_RPC_TIMEOUT:-120}" \
    --rpc-retries "${VEGETA_S4_RPC_RETRIES:-5}" \
    --rpc-backoff "${VEGETA_S4_RPC_BACKOFF:-1.0}" \
    --delay-ms "${VEGETA_S4_CODE_DELAY_MS:-20}" \
    --checkpoint-every "${VEGETA_S4_CODE_CHECKPOINT_EVERY:-100}"

  # One built-in callTracer trace per block, resumably checkpointed. This is needed to discover
  # internal call/delegatecall targets for later native family mapping; it is not SLOAD/SSTORE tracing.
  python3 tools/vegeta/collect-vegeta-calltraces.py \
    --corpus "$WORK_DIR/thin-corpus.jsonl" \
    --cache-dir "$WORK_DIR/call-cache" \
    --relevant-addresses "$WORK_DIR/relevant-addresses.json" \
    --rpc-url "$RPC" \
    --rpc-timeout "${VEGETA_S4_CALL_RPC_TIMEOUT:-600}" \
    --rpc-retries "${VEGETA_S4_RPC_RETRIES:-5}" \
    --rpc-backoff "${VEGETA_S4_RPC_BACKOFF:-1.0}" \
    --delay-ms "${VEGETA_S4_CALL_DELAY_MS:-50}" \
    --trace-timeout "${VEGETA_S4_CALL_TRACE_TIMEOUT:-600}" \
    --reexec "${VEGETA_S4_CALL_REEXEC:-128}"

  # Pass 2: callTracer may have added internal targets to relevant-addresses.json.
  python3 tools/vegeta/fetch-vegeta-historical-code.py \
    --addresses "$WORK_DIR/relevant-addresses.json" \
    --output "$WORK_DIR/code-cache.json" \
    --rpc-url "$RPC" \
    --rpc-timeout "${VEGETA_S4_CODE_RPC_TIMEOUT:-120}" \
    --rpc-retries "${VEGETA_S4_RPC_RETRIES:-5}" \
    --rpc-backoff "${VEGETA_S4_RPC_BACKOFF:-1.0}" \
    --delay-ms "${VEGETA_S4_CODE_DELAY_MS:-20}" \
    --checkpoint-every "${VEGETA_S4_CODE_CHECKPOINT_EVERY:-100}"
fi

echo
echo "PASS: Vegeta S4 native-characterization inputs are frozen"
echo "thin corpus:        $WORK_DIR/thin-corpus.jsonl"
echo "source summary:     $WORK_DIR/source-summary.json"
echo "relevant addresses: $WORK_DIR/relevant-addresses.json"
echo "historical code:    $WORK_DIR/code-cache.json"
echo "callTracer cache:   $WORK_DIR/call-cache/"
echo
echo "No exact custom-JS SLOAD/SSTORE traces were collected."
echo "Next step is S4 family/semantic characterization using these frozen caches."
