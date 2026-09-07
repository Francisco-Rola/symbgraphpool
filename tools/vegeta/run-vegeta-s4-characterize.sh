#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
BASE_MAP="${VEGETA_S4_BASE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
CANDIDATE_MAP="${VEGETA_S4_CANDIDATE_FAMILY_MAP:-$WORK_DIR/s4-native-family-map.candidate.json}"
REVIEW_SEED_TEMPLATE="${VEGETA_S4_REVIEW_SEED_TEMPLATE:-evaluation/vegeta/s4-review-seeds.v1.json}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
REVIEW_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
REVIEWED_DRAFT="${VEGETA_S4_REVIEW_DRAFT_MAP:-$WORK_DIR/s4-native-family-map.reviewed-draft.json}"
TOP="${VEGETA_S4_CHARACTERIZE_TOP:-100}"

for path in \
  "$CORPUS" \
  "$WORK_DIR/thin-corpus.jsonl" \
  "$WORK_DIR/source-summary.json" \
  "$WORK_DIR/relevant-addresses.json" \
  "$WORK_DIR/code-cache.json" \
  "$BASE_MAP" \
  "$REVIEW_SEED_TEMPLATE"; do
  [[ -s "$path" ]] || { echo "missing frozen S4 characterization input: $path" >&2; exit 2; }
done
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing frozen S4 callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

python3 - "$WORK_DIR/source-summary.json" <<'PY_SUMMARY'
import json,sys
d=json.load(open(sys.argv[1]))
expected=(5000,18581726,18586725)
observed=(int(d.get('blocks',0)),int(d.get('first_block',-1)),int(d.get('last_block',-1)))
if observed != expected:
    raise SystemExit(f"S4 frozen source range mismatch: observed blocks/first/last={observed}, expected={expected}")
if int(d.get('transactions',0)) <= 0:
    raise SystemExit('S4 frozen source summary has no transactions')
print(f"S4 frozen source validated: blocks={observed[0]} range={observed[1]}..{observed[2]} tx={d.get('transactions')}")
PY_SUMMARY


python3 tools/vegeta/audit-vegeta-s4-corpus-provenance.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --output "$WORK_DIR/corpus-provenance.json" \
  --text-output "$WORK_DIR/corpus-provenance.txt"

# Purely local characterization. No ETH_RPC_URL is read by this stage.
python3 tools/vegeta/characterize-vegeta-s4-frozen.py \
  --corpus "$CORPUS" \
  --thin-corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --relevant-addresses "$WORK_DIR/relevant-addresses.json" \
  --output-dir "$WORK_DIR" \
  --top "$TOP"

# Identical historical runtime bytecode may reuse already-reviewed S1/S3 native-family mappings.
# This creates only a candidate map. New S4 families/selectors remain explicitly unmapped.
python3 tools/vegeta/bootstrap-vegeta-s4-family-map.py \
  --base-map "$BASE_MAP" \
  --family-summary "$WORK_DIR/family-summary.json" \
  --output "$CANDIDATE_MAP"

if [[ ! -e "$REVIEW_BASE" ]]; then
  cp "$CANDIDATE_MAP" "$REVIEW_BASE"
fi
if [[ ! -e "$REVIEW_DECISIONS" ]]; then
  cp "$REVIEW_SEED_TEMPLATE" "$REVIEW_DECISIONS"
fi

# Always regenerate the reviewed draft from the persistent review base + human decision file.
# The review base may add a genuinely new native implementation/profile (for example a custom router)
# and is never overwritten by rerunning characterization. Pending decisions remain non-executable.
python3 tools/vegeta/apply-vegeta-s4-review-decisions.py \
  --base-map "$REVIEW_BASE" \
  --decisions "$REVIEW_DECISIONS" \
  --output "$REVIEWED_DRAFT"

python3 tools/vegeta/build-vegeta-delegate-resolutions.py \
  --corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --family-map "$REVIEWED_DRAFT" \
  --output "$WORK_DIR/native-family-mapping-candidates.json"

python3 tools/vegeta/audit-vegeta-native-family-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$REVIEWED_DRAFT" \
  --output "$WORK_DIR/source-family-coverage.json" \
  --text-output "$WORK_DIR/source-family-coverage.txt" \
  --top-unmapped "${VEGETA_S4_COVERAGE_TOP_UNMAPPED:-200}"

python3 tools/vegeta/plan-vegeta-s4-semantic-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$REVIEWED_DRAFT" \
  --review-seeds "$REVIEW_DECISIONS" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --clusters-output "$WORK_DIR/family-blocker-clusters.json" \
  --plan-output "$WORK_DIR/family-marginal-coverage.json" \
  --text-output "$WORK_DIR/family-marginal-coverage.txt" \
  --top-clusters "${VEGETA_S4_COVERAGE_PLAN_TOP_CLUSTERS:-100}" \
  --top-families "${VEGETA_S4_COVERAGE_PLAN_TOP_FAMILIES:-100}" \
  --max-greedy-steps "${VEGETA_S4_COVERAGE_PLAN_MAX_STEPS:-100}" \
  --target-conflict-relevant-access "${VEGETA_S4_MIN_CONFLICT_RELEVANT_ACCESS_COVERAGE:-${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-${VEGETA_S4_MIN_FAMILY_STATE_GAS_COVERAGE:-0.90}}}" \
  --target-conflict "${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"

python3 tools/vegeta/build-vegeta-s4-review-queue.py \
  --family-summary "$WORK_DIR/family-summary.json" \
  --selector-summary "$WORK_DIR/selector-summary.json" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --review-seeds "$REVIEW_DECISIONS" \
  --output-json "$WORK_DIR/family-review-queue.json" \
  --output-md "$WORK_DIR/family-review-queue.md" \
  --top "${VEGETA_S4_REVIEW_TOP:-50}"

python3 tools/vegeta/check-vegeta-s4-family-review-readiness.py \
  --family-map "$REVIEWED_DRAFT" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --provenance "$WORK_DIR/corpus-provenance.json" \
  --min-conflict "${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}" \
  --min-median-block "${VEGETA_S4_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}" \
  --min-conflict-relevant-access "${VEGETA_S4_MIN_CONFLICT_RELEVANT_ACCESS_COVERAGE:-${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-${VEGETA_S4_MIN_FAMILY_STATE_GAS_COVERAGE:-0.90}}}" \
  --output "$WORK_DIR/family-review-readiness.json" \
  --text-output "$WORK_DIR/family-review-readiness.txt" \
  --allow-low

echo
echo "PASS: Vegeta S4 frozen family characterization complete"
echo "corpus provenance:     $WORK_DIR/corpus-provenance.txt"
echo "family summary:       $WORK_DIR/family-summary.txt"
echo "selector summary:     $WORK_DIR/selector-summary.json"
echo "known-family coverage:$WORK_DIR/source-family-coverage.txt"
echo "coverage blocker plan: $WORK_DIR/family-marginal-coverage.txt"
echo "blocker clusters:      $WORK_DIR/family-blocker-clusters.json"
echo "semantic review queue:$WORK_DIR/family-review-queue.md"
echo "candidate family map: $CANDIDATE_MAP"
echo "review base map:      $REVIEW_BASE"
echo "review decisions:     $REVIEW_DECISIONS"
echo "reviewed draft map:   $REVIEWED_DRAFT"
echo "family freeze gate:   $WORK_DIR/family-review-readiness.txt"
echo
echo "Next step: inspect $WORK_DIR/family-marginal-coverage.txt, then edit $REVIEW_DECISIONS for the first review batch."
echo "For each completed row set review_status=reviewed, reviewed_native_family, and mapping_basis."
echo "Then rerun this command (or tools/vegeta/run-vegeta-s4-review-check.sh) to recompute exact conflict coverage; access/gas metrics remain diagnostics."
echo "Only after family-review-readiness PASS should you freeze evaluation/vegeta/s4-native-family-map.v1.json and run prepare-native."
