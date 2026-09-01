#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
S3_CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
RPC="${ETH_RPC_URL:-}"
if [[ "${VEGETA_S1_REUSE_CACHED_COVERAGE_INPUTS:-0}" == "1" ]]; then RPC=""; fi

[[ -s "$CORPUS" ]] || { echo "missing Vegeta S1 corpus: $CORPUS" >&2; exit 2; }
[[ -s "$FAMILY_MAP" ]] || { echo "missing S1 native family map: $FAMILY_MAP" >&2; exit 2; }
mkdir -p "$WORK_DIR/call-cache"

python3 tools/vegeta/prepare-vegeta-s1-inputs.py --corpus "$CORPUS" --output-dir "$WORK_DIR"

if [[ -s "$S3_CORPUS" ]]; then
  python3 tools/vegeta/validate-vegeta-s1-prefix.py \
    --s1-corpus "$CORPUS" --s3-corpus "$S3_CORPUS" --prefix-blocks 101
else
  echo "NOTE: S3 corpus not present; skipping exact 101-block source-prefix regression check" >&2
fi

if [[ -n "$RPC" ]]; then
  # Fetch direct/storage-owner bytecode first. callTracer then discovers internal call targets; the
  # second code pass fills those new addresses. All stages are resumable.
  python3 tools/vegeta/fetch-vegeta-historical-code.py \
    --addresses "$WORK_DIR/relevant-addresses.json" \
    --output "$WORK_DIR/code-cache.json" \
    --rpc-url "$RPC" \
    --delay-ms "${VEGETA_S1_CODE_DELAY_MS:-20}"

  python3 tools/vegeta/collect-vegeta-calltraces.py \
    --corpus "$WORK_DIR/thin-corpus.jsonl" \
    --cache-dir "$WORK_DIR/call-cache" \
    --relevant-addresses "$WORK_DIR/relevant-addresses.json" \
    --rpc-url "$RPC" \
    --delay-ms "${VEGETA_S1_CALL_DELAY_MS:-50}" \
    --trace-timeout "${VEGETA_S1_CALL_TRACE_TIMEOUT:-600}" \
    --reexec "${VEGETA_S1_CALL_REEXEC:-128}"

  python3 tools/vegeta/fetch-vegeta-historical-code.py \
    --addresses "$WORK_DIR/relevant-addresses.json" \
    --output "$WORK_DIR/code-cache.json" \
    --rpc-url "$RPC" \
    --delay-ms "${VEGETA_S1_CODE_DELAY_MS:-20}"
else
  [[ -s "$WORK_DIR/code-cache.json" ]] || { echo "ETH_RPC_URL is unset and cached code is missing: $WORK_DIR/code-cache.json" >&2; exit 2; }
  [[ -d "$WORK_DIR/call-cache" ]] || { echo "ETH_RPC_URL is unset and cached call traces are missing: $WORK_DIR/call-cache" >&2; exit 2; }
  echo "S1 coverage refresh: reusing completed code/callTracer caches; no RPC calls"
fi

python3 tools/vegeta/build-vegeta-delegate-resolutions.py \
  --corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --family-map "$FAMILY_MAP" \
  --output "$WORK_DIR/native-family-mapping-candidates.json"

python3 tools/vegeta/audit-vegeta-native-family-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$FAMILY_MAP" \
  --output "$WORK_DIR/source-family-coverage.json" \
  --text-output "$WORK_DIR/source-family-coverage.txt" \
  --top-unmapped "${VEGETA_S1_COVERAGE_TOP_UNMAPPED:-200}"

echo
echo "PASS: Vegeta S1 native-family coverage audit complete"
echo "coverage: $WORK_DIR/source-family-coverage.txt"
echo "callTracer cache: $WORK_DIR/call-cache"
echo "next: bash tools/legacy-scripts/run-vegeta-s1-family-expansion-plan.sh"
