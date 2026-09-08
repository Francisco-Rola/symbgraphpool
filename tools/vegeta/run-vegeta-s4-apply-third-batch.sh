#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
FAMILY_EXTENSION="${VEGETA_S4_THIRD_BATCH_FAMILY_EXTENSION:-evaluation/vegeta/s4-third-batch-native-family-extension.v1.json}"
THIRD_BATCH="${VEGETA_S4_THIRD_BATCH_REVIEWED_DECISIONS:-evaluation/vegeta/s4-third-batch-reviewed-decisions.v1.json}"
PENDING_OUT="${VEGETA_S4_THIRD_BATCH_PENDING_OUT:-$WORK_DIR/s4-third-batch-pending-review.json}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_STORAGE_ACCESS="${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}"

for path in "$REVIEW_BASE" "$WORKSPACE_DECISIONS" "$FAMILY_EXTENSION" "$THIRD_BATCH"; do
  [[ -s "$path" ]] || { echo "missing S4 third-batch input: $path" >&2; exit 2; }
done

python3 - "$WORKSPACE_DECISIONS" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
reviewed={int(r.get('priority',-1)) for r in d.get('decisions',[]) if str(r.get('review_status','')).lower()=='reviewed'}
missing=sorted(set(range(1,12))-reviewed)
if missing:
    raise SystemExit(
        f"third batch expects the first and second reviewed batches already installed; "
        f"missing reviewed priorities: {missing}. Run first-batch and second-batch wrappers first."
    )
PY

python3 tools/vegeta/apply-vegeta-s4-third-batch.py \
  --review-base "$REVIEW_BASE" \
  --workspace-decisions "$WORKSPACE_DECISIONS" \
  --family-extension "$FAMILY_EXTENSION" \
  --third-batch "$THIRD_BATCH" \
  --pending-output "$PENDING_OUT"

# Exact checkpoint: allow the wrapper itself to complete below 95%, because conflict-closure.txt is
# the authoritative instruction for whether one more small review batch is necessary.
VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh

python3 tools/vegeta/summarize-vegeta-s4-conflict-closure.py \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --readiness "$WORK_DIR/family-review-readiness.json" \
  --decisions "$WORKSPACE_DECISIONS" \
  --min-conflict "$MIN_CONFLICT" \
  --min-storage-access "$MIN_STORAGE_ACCESS" \
  --output "$WORK_DIR/conflict-closure.json" \
  --text-output "$WORK_DIR/conflict-closure.txt"

echo
echo "PASS: applied S4 third conflict-closure batch and recomputed exact coverage"
echo "decisions: $WORKSPACE_DECISIONS"
echo "pending:   $PENDING_OUT"
echo "coverage:  $WORK_DIR/source-family-coverage.txt"
echo "gate:      $WORK_DIR/family-review-readiness.txt"
echo "closure:   $WORK_DIR/conflict-closure.txt"
echo
if grep -q '^family freeze gate: PASS' "$WORK_DIR/conflict-closure.txt"; then
  echo "Dual publication/family gate is closed. Next:"
  echo "  VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh"
else
  echo "Publication/family gate is still open. Do NOT freeze. Use the balanced planner for the remaining access/conflict deficits."
  echo "AMP/bridge/rollup families remain intentionally pending unless explicit semantics are reviewed; access-heavy families may also be required after conflict closure."
fi
