#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
CANDIDATES="${VEGETA_S4_FIFTH_BATCH_CANDIDATES_JSON:-$WORK_DIR/s4-fifth-batch-review-candidates.json}"
OUTPUT_JSON="${VEGETA_S4_FIFTH_BATCH_EVIDENCE_JSON:-$WORK_DIR/s4-fifth-batch-evidence.json}"
OUTPUT_MD="${VEGETA_S4_FIFTH_BATCH_EVIDENCE_MD:-$WORK_DIR/s4-fifth-batch-evidence.md}"
SOURCE_CACHE="${VEGETA_S4_FIFTH_BATCH_SOURCE_CACHE:-$WORK_DIR/s4-fifth-batch-source-resolution-cache.json}"
FAMILY_MAP="${VEGETA_S4_REVIEW_DRAFT_MAP:-$WORK_DIR/s4-native-family-map.reviewed-draft.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"

for path in \
  "$CANDIDATES" "$CORPUS" "$WORK_DIR/code-cache.json" "$WORK_DIR/family-summary.json" \
  "$WORK_DIR/selector-summary.json" "$WORK_DIR/family-blocker-clusters.json" \
  "$WORK_DIR/source-family-coverage.json" "$WORK_DIR/native-family-mapping-candidates.json" \
  "$FAMILY_MAP" "$WORKSPACE_DECISIONS"; do
  [[ -s "$path" ]] || { echo "missing S4 fifth-batch analysis input: $path" >&2; echo "Run: bash tools/vegeta/run-vegeta-s4-plan-fifth-batch.sh" >&2; exit 2; }
done
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing S4 callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

ARGS=(
  python3 tools/vegeta/build-vegeta-s4-fifth-batch-evidence.py
  --candidates "$CANDIDATES"
  --corpus "$CORPUS"
  --call-cache "$WORK_DIR/call-cache"
  --code-cache "$WORK_DIR/code-cache.json"
  --family-summary "$WORK_DIR/family-summary.json"
  --selector-summary "$WORK_DIR/selector-summary.json"
  --clusters "$WORK_DIR/family-blocker-clusters.json"
  --coverage "$WORK_DIR/source-family-coverage.json"
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json"
  --family-map "$FAMILY_MAP"
  --workspace-decisions "$WORKSPACE_DECISIONS"
  --output "$OUTPUT_JSON"
  --text-output "$OUTPUT_MD"
  --source-cache "$SOURCE_CACHE"
  --representative-transactions "${VEGETA_S4_FIFTH_EVIDENCE_TX_SAMPLES:-12}"
  --top-selectors "${VEGETA_S4_FIFTH_EVIDENCE_TOP_SELECTORS:-30}"
  --top-clusters-per-family "${VEGETA_S4_FIFTH_EVIDENCE_TOP_CLUSTERS:-12}"
  --top-co-blockers "${VEGETA_S4_FIFTH_EVIDENCE_TOP_COBLOCKERS:-20}"
)
if [[ "${VEGETA_S4_FIFTH_FETCH_SOURCE:-0}" == "1" ]]; then
  ARGS+=(--fetch-source)
  if [[ "${VEGETA_S4_FIFTH_REFRESH_SOURCE:-0}" == "1" ]]; then ARGS+=(--refresh-source-cache); fi
fi
"${ARGS[@]}"

echo
echo "PASS: fifth-batch semantic evidence dossier generated (analysis only; no mappings applied)"
echo "markdown: $OUTPUT_MD"
echo "json:     $OUTPUT_JSON"
if [[ "${VEGETA_S4_FIFTH_FETCH_SOURCE:-0}" == "1" ]]; then
  echo "sources:  $SOURCE_CACHE"
else
  echo "source enrichment was not requested. For verified metadata, rerun:"
  echo "  VEGETA_S4_FIFTH_FETCH_SOURCE=1 bash tools/vegeta/run-vegeta-s4-analyze-fifth-batch.sh"
  echo "Etherscan fallback is used when ETHERSCAN_API_KEY is set; Sourcify requires no key."
fi

echo
echo "Send s4-fifth-batch-evidence.md (and the JSON if practical) for semantic classification."
