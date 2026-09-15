#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
FAMILY_EXTENSION="${VEGETA_S4_FOURTH_BATCH_FAMILY_EXTENSION:-evaluation/vegeta/s4-fourth-batch-native-family-extension.v1.json}"
FOURTH_BATCH="${VEGETA_S4_FOURTH_BATCH_REVIEWED_DECISIONS:-evaluation/vegeta/s4-fourth-batch-reviewed-decisions.v1.json}"
REVIEW_EVIDENCE="${VEGETA_S4_FOURTH_BATCH_REVIEW_EVIDENCE:-evaluation/vegeta/s4-fourth-batch-review-evidence.v1.json}"
THIRD_BATCH="${VEGETA_S4_THIRD_BATCH_REVIEWED_DECISIONS:-evaluation/vegeta/s4-third-batch-reviewed-decisions.v1.json}"
PENDING_OUT="${VEGETA_S4_FOURTH_BATCH_PENDING_OUT:-$WORK_DIR/s4-fourth-batch-pending-review.json}"
PRE_COVERAGE="${VEGETA_S4_FOURTH_BATCH_PRE_COVERAGE:-$WORK_DIR/s4-fourth-batch-pre-source-family-coverage.json}"
DELTA_JSON="${VEGETA_S4_FOURTH_BATCH_DELTA_JSON:-$WORK_DIR/s4-fourth-batch-coverage-delta.json}"
DELTA_TXT="${VEGETA_S4_FOURTH_BATCH_DELTA_TXT:-$WORK_DIR/s4-fourth-batch-coverage-delta.txt}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
DIAG_STORAGE_ACCESS_REFERENCE="${VEGETA_S4_STORAGE_ACCESS_REFERENCE:-${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}}"

for path in "$REVIEW_BASE" "$WORKSPACE_DECISIONS" "$FAMILY_EXTENSION" "$FOURTH_BATCH" "$REVIEW_EVIDENCE" "$THIRD_BATCH"; do
  [[ -s "$path" ]] || { echo "missing S4 fourth-batch input: $path" >&2; exit 2; }
done

# Suggestions 2-4 are deliberately one fail-closed workflow: verify the third reviewed batch is
# already installed, recompute an exact pre-batch baseline, apply only evidence-backed P12-P16
# decisions, then recompute exact coverage and report the observed delta.
python3 - "$WORKSPACE_DECISIONS" "$THIRD_BATCH" <<'PY'
import json,sys
workspace=json.load(open(sys.argv[1])); third=json.load(open(sys.argv[2]))
reviewed={str(r.get('runtime_code_family') or '') for r in workspace.get('decisions',[]) if str(r.get('review_status','')).lower()=='reviewed'}
required={str(r.get('runtime_code_family') or '') for r in third.get('decisions',[])}
missing=sorted(required-reviewed)
if missing:
    raise SystemExit(
        "fourth batch expects the checked-in third batch already installed; missing reviewed runtime families: "
        + ", ".join(missing)
    )
PY

VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh
cp "$WORK_DIR/source-family-coverage.json" "$PRE_COVERAGE"

python3 tools/vegeta/apply-vegeta-s4-fourth-batch.py \
  --review-base "$REVIEW_BASE" \
  --workspace-decisions "$WORKSPACE_DECISIONS" \
  --family-extension "$FAMILY_EXTENSION" \
  --fourth-batch "$FOURTH_BATCH" \
  --review-evidence "$REVIEW_EVIDENCE" \
  --pending-output "$PENDING_OUT"

VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh

python3 tools/vegeta/summarize-vegeta-s4-conflict-closure.py \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --readiness "$WORK_DIR/family-review-readiness.json" \
  --decisions "$WORKSPACE_DECISIONS" \
  --min-conflict "$MIN_CONFLICT" \
  --storage-access-reference "$DIAG_STORAGE_ACCESS_REFERENCE" \
  --output "$WORK_DIR/conflict-closure.json" \
  --text-output "$WORK_DIR/conflict-closure.txt"

python3 tools/vegeta/summarize-vegeta-s4-batch-delta.py \
  --before "$PRE_COVERAGE" \
  --after "$WORK_DIR/source-family-coverage.json" \
  --batch-label "fourth reviewed conflict batch P12-P16" \
  --target-conflict "$MIN_CONFLICT" \
  --target-storage-access "$DIAG_STORAGE_ACCESS_REFERENCE" \
  --output "$DELTA_JSON" \
  --text-output "$DELTA_TXT"

echo
echo "PASS: applied evidence-backed S4 fourth batch and recomputed exact before/after coverage"
echo "decisions: $WORKSPACE_DECISIONS"
echo "evidence:  $REVIEW_EVIDENCE"
echo "pending:   $PENDING_OUT"
echo "coverage:  $WORK_DIR/source-family-coverage.txt"
echo "gate:      $WORK_DIR/family-review-readiness.txt"
echo "closure:   $WORK_DIR/conflict-closure.txt"
echo "delta:     $DELTA_TXT"
echo
if grep -q '^family freeze gate: PASS' "$WORK_DIR/conflict-closure.txt"; then
  echo "Both family publication gates are closed. Inspect the evidence/delta, then freeze only with explicit review attestation:"
  echo "  VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh"
else
  echo "At least one family publication gate is still open. Do NOT freeze. Recompute the balanced/access-first plan from the regenerated family-marginal-coverage.txt."
fi
