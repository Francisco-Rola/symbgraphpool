#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
REVIEW_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
DRAFT_MAP="${VEGETA_S4_REVIEW_DRAFT_MAP:-$WORK_DIR/s4-native-family-map.reviewed-draft.json}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_MEDIAN="${VEGETA_S4_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}"
MIN_STORAGE_ACCESS="${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}"
DIAG_CONFLICT_RELEVANT_ACCESS_REFERENCE="${VEGETA_S4_MIN_CONFLICT_RELEVANT_ACCESS_COVERAGE:-0.90}"

for path in "$CORPUS" "$WORK_DIR/code-cache.json" "$WORK_DIR/thin-corpus.jsonl" "$WORK_DIR/corpus-provenance.json" "$REVIEW_BASE" "$REVIEW_DECISIONS"; do
  [[ -s "$path" ]] || { echo "missing S4 review-check input: $path" >&2; exit 2; }
done
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing S4 callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

python3 tools/vegeta/apply-vegeta-s4-review-decisions.py \
  --base-map "$REVIEW_BASE" \
  --decisions "$REVIEW_DECISIONS" \
  --output "$DRAFT_MAP"

python3 tools/vegeta/build-vegeta-delegate-resolutions.py \
  --corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --family-map "$DRAFT_MAP" \
  --output "$WORK_DIR/native-family-mapping-candidates.json"

python3 tools/vegeta/audit-vegeta-native-family-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$DRAFT_MAP" \
  --output "$WORK_DIR/source-family-coverage.json" \
  --text-output "$WORK_DIR/source-family-coverage.txt" \
  --top-unmapped "${VEGETA_S4_COVERAGE_TOP_UNMAPPED:-200}"

python3 tools/vegeta/plan-vegeta-s4-semantic-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$DRAFT_MAP" \
  --review-seeds "$REVIEW_DECISIONS" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --clusters-output "$WORK_DIR/family-blocker-clusters.json" \
  --plan-output "$WORK_DIR/family-marginal-coverage.json" \
  --text-output "$WORK_DIR/family-marginal-coverage.txt" \
  --top-clusters "${VEGETA_S4_COVERAGE_PLAN_TOP_CLUSTERS:-100}" \
  --top-families "${VEGETA_S4_COVERAGE_PLAN_TOP_FAMILIES:-100}" \
  --max-greedy-steps "${VEGETA_S4_COVERAGE_PLAN_MAX_STEPS:-100}" \
  --target-storage-access "$MIN_STORAGE_ACCESS" \
  --target-conflict-relevant-access "$DIAG_CONFLICT_RELEVANT_ACCESS_REFERENCE" \
  --target-conflict "$MIN_CONFLICT"

python3 tools/vegeta/build-vegeta-s4-review-queue.py \
  --family-summary "$WORK_DIR/family-summary.json" \
  --selector-summary "$WORK_DIR/selector-summary.json" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --review-seeds "$REVIEW_DECISIONS" \
  --output-json "$WORK_DIR/family-review-queue.json" \
  --output-md "$WORK_DIR/family-review-queue.md" \
  --top "${VEGETA_S4_REVIEW_TOP:-50}"

CHECK=(
  python3 tools/vegeta/check-vegeta-s4-family-review-readiness.py
  --family-map "$DRAFT_MAP"
  --coverage "$WORK_DIR/source-family-coverage.json"
  --provenance "$WORK_DIR/corpus-provenance.json"
  --min-conflict "$MIN_CONFLICT"
  --min-median-block "$MIN_MEDIAN"
  --min-storage-access "$MIN_STORAGE_ACCESS"
  --min-conflict-relevant-access "$DIAG_CONFLICT_RELEVANT_ACCESS_REFERENCE"
  --output "$WORK_DIR/family-review-readiness.json"
  --text-output "$WORK_DIR/family-review-readiness.txt"
)
if [[ "${VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE:-0}" == "1" ]]; then CHECK+=(--allow-low); fi
"${CHECK[@]}"

echo
echo "Review check complete."
echo "coverage: $WORK_DIR/source-family-coverage.txt"
echo "planner:  $WORK_DIR/family-marginal-coverage.txt"
echo "clusters: $WORK_DIR/family-blocker-clusters.json"
echo "queue:    $WORK_DIR/family-review-queue.md"
echo "gate:     $WORK_DIR/family-review-readiness.txt"
echo
echo "If the gate fails, use the dual-gate planner first; prioritize normalized closure of the remaining all-storage-access and unique-conflict deficits. Strict-gas complement lookahead is diagnostic only."
echo "For the full storage tail / next conflict review prefix, run: bash tools/vegeta/run-vegeta-s4-plan-fifth-batch.sh"
echo "If PASS and the draft is manually reviewed, freeze with: VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh"
