#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
CANDIDATES_JSON="${VEGETA_S4_FIFTH_BATCH_CANDIDATES_JSON:-$WORK_DIR/s4-fifth-batch-review-candidates.json}"
CANDIDATES_TXT="${VEGETA_S4_FIFTH_BATCH_CANDIDATES_TXT:-$WORK_DIR/s4-fifth-batch-review-candidates.txt}"
DECISIONS_DRAFT="${VEGETA_S4_FIFTH_BATCH_DECISIONS:-$WORK_DIR/s4-fifth-batch-review-decisions.draft.json}"

# Dedicated wide-tail planning pass. Keep the normal review-check defaults modest; this command is
# for answering how long the 90% semantic-surface tail really is and for selecting the exact next
# conflict-review prefix. Defaults are intentionally wider than the earlier 1000-family pass because
# that pass projected only ~88.5% all-storage coverage. The planner itself still recomputes conflict-pair unions after each step.
VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 \
VEGETA_S4_COVERAGE_PLAN_TOP_CLUSTERS="${VEGETA_S4_WIDE_PLAN_TOP_CLUSTERS:-1000}" \
VEGETA_S4_COVERAGE_PLAN_TOP_FAMILIES="${VEGETA_S4_WIDE_PLAN_TOP_FAMILIES:-2000}" \
VEGETA_S4_COVERAGE_PLAN_MAX_STEPS="${VEGETA_S4_WIDE_PLAN_MAX_STEPS:-2000}" \
VEGETA_S4_REVIEW_TOP="${VEGETA_S4_WIDE_REVIEW_TOP:-500}" \
bash tools/vegeta/run-vegeta-s4-review-check.sh

ARGS=(
  python3 tools/vegeta/build-vegeta-s4-fifth-batch-review-scaffold.py
  --plan "$WORK_DIR/family-marginal-coverage.json"
  --review-queue "$WORK_DIR/family-review-queue.json"
  --workspace-decisions "$WORKSPACE_DECISIONS"
  --output "$CANDIDATES_JSON"
  --text-output "$CANDIDATES_TXT"
  --decisions-draft "$DECISIONS_DRAFT"
  --access-tail-top "${VEGETA_S4_FIFTH_ACCESS_TAIL_TOP:-40}"
)
if [[ "${VEGETA_S4_FIFTH_REGENERATE:-0}" == "1" ]]; then
  ARGS+=(--force-decisions-draft)
fi
"${ARGS[@]}"

echo
echo "PASS: wide S4 dual-gate plan and fifth-batch review scaffold generated"
echo "planner:    $WORK_DIR/family-marginal-coverage.txt"
echo "candidates: $CANDIDATES_TXT"
echo "decisions:  $DECISIONS_DRAFT"
echo
echo "The decisions file is intentionally pending/non-executable. Next generate the local evidence dossier:"
echo "  bash tools/vegeta/run-vegeta-s4-analyze-fifth-batch.sh"
echo "Optionally enrich representative addresses with verified source metadata:"
echo "  VEGETA_S4_FIFTH_FETCH_SOURCE=1 bash tools/vegeta/run-vegeta-s4-analyze-fifth-batch.sh"
echo "Review identities and semantics, add any required native dependency family to the S4 review base, then fill"
echo "reviewed_native_family, mapping_basis, review_conclusion, and evidence_sources before applying the batch."
