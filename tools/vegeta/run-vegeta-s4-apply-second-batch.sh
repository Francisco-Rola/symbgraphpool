#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
SECOND_BATCH="${VEGETA_S4_SECOND_BATCH_REVIEW_CANDIDATES:-evaluation/vegeta/s4-second-batch-review-candidates.v1.json}"
PENDING_OUT="${VEGETA_S4_SECOND_BATCH_PENDING_OUT:-$WORK_DIR/s4-second-batch-pending-review.json}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
DIAG_STORAGE_ACCESS_REFERENCE="${VEGETA_S4_STORAGE_ACCESS_REFERENCE:-${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}}"

for path in "$REVIEW_BASE" "$WORKSPACE_DECISIONS" "$SECOND_BATCH"; do
  [[ -s "$path" ]] || { echo "missing S4 second-batch input: $path" >&2; exit 2; }
done

python3 - "$WORKSPACE_DECISIONS" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
reviewed={int(r.get('priority',-1)) for r in d.get('decisions',[]) if str(r.get('review_status','')).lower()=='reviewed'}
missing=sorted(set(range(1,7))-reviewed)
if missing:
    raise SystemExit(f"second batch expects first reviewed batch already installed; missing reviewed priorities: {missing}. Run tools/vegeta/run-vegeta-s4-apply-first-batch.sh first.")
PY

python3 tools/vegeta/apply-vegeta-s4-second-batch.py \
  --review-base "$REVIEW_BASE" \
  --workspace-decisions "$WORKSPACE_DECISIONS" \
  --second-batch "$SECOND_BATCH" \
  --pending-output "$PENDING_OUT"

# This is an exact checkpoint. It is allowed to remain below 95% after the safe aliases; the
# generated conflict-closure report then tells us precisely how much conflict evidence remains.
VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh

python3 tools/vegeta/summarize-vegeta-s4-conflict-closure.py \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --readiness "$WORK_DIR/family-review-readiness.json" \
  --decisions "$WORKSPACE_DECISIONS" \
  --min-conflict "$MIN_CONFLICT" \
  --storage-access-reference "$DIAG_STORAGE_ACCESS_REFERENCE" \
  --output "$WORK_DIR/conflict-closure.json" \
  --text-output "$WORK_DIR/conflict-closure.txt"

echo
echo "PASS: applied S4 second conflict-closure batch and recomputed exact coverage"
echo "decisions: $WORKSPACE_DECISIONS"
echo "pending:   $PENDING_OUT"
echo "coverage:  $WORK_DIR/source-family-coverage.txt"
echo "gate:      $WORK_DIR/family-review-readiness.txt"
echo "closure:   $WORK_DIR/conflict-closure.txt"
echo
echo "If either publication gate is still open, use family-marginal-coverage.txt / conflict-closure.txt to close the remaining all-storage-access and conflict deficits."
echo "If the gate is PASS, freeze with: VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh"
