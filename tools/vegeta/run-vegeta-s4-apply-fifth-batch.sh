#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
FIFTH_BATCH="${VEGETA_S4_FIFTH_BATCH_DECISIONS:-evaluation/vegeta/s4-fifth-batch-reviewed-decisions.v1.json}"
FIFTH_EXTENSION="${VEGETA_S4_FIFTH_BATCH_NATIVE_EXTENSION:-evaluation/vegeta/s4-fifth-batch-native-family-extension.v1.json}"
FIFTH_REVIEW_EVIDENCE="${VEGETA_S4_FIFTH_BATCH_REVIEW_EVIDENCE:-evaluation/vegeta/s4-fifth-batch-review-evidence.v1.json}"
GENERATED_EVIDENCE="${VEGETA_S4_FIFTH_BATCH_GENERATED_EVIDENCE:-$WORK_DIR/s4-fifth-batch-evidence.json}"
FOURTH_BATCH="${VEGETA_S4_FOURTH_BATCH_REVIEWED_DECISIONS:-evaluation/vegeta/s4-fourth-batch-reviewed-decisions.v1.json}"
PENDING_OUT="${VEGETA_S4_FIFTH_BATCH_PENDING_OUT:-$WORK_DIR/s4-fifth-batch-pending-review.json}"
PRE_COVERAGE="${VEGETA_S4_FIFTH_BATCH_PRE_COVERAGE:-$WORK_DIR/s4-fifth-batch-pre-source-family-coverage.json}"
DELTA_JSON="${VEGETA_S4_FIFTH_BATCH_DELTA_JSON:-$WORK_DIR/s4-fifth-batch-coverage-delta.json}"
DELTA_TXT="${VEGETA_S4_FIFTH_BATCH_DELTA_TXT:-$WORK_DIR/s4-fifth-batch-coverage-delta.txt}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
DIAG_STORAGE_ACCESS_REFERENCE="${VEGETA_S4_STORAGE_ACCESS_REFERENCE:-${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}}"

for path in "$REVIEW_BASE" "$WORKSPACE_DECISIONS" "$FIFTH_BATCH" "$FIFTH_EXTENSION" "$FIFTH_REVIEW_EVIDENCE" "$GENERATED_EVIDENCE" "$FOURTH_BATCH"; do
  [[ -s "$path" ]] || { echo "missing S4 fifth-batch input: $path" >&2; exit 2; }
done

python3 - "$WORKSPACE_DECISIONS" "$FOURTH_BATCH" <<'PY'
import json,sys
workspace=json.load(open(sys.argv[1])); fourth=json.load(open(sys.argv[2]))
reviewed={str(r.get('runtime_code_family') or '') for r in workspace.get('decisions',[]) if str(r.get('review_status','')).lower()=='reviewed'}
required={str(r.get('runtime_code_family') or '') for r in fourth.get('decisions',[])}
missing=sorted(required-reviewed)
if missing:
    raise SystemExit("fifth batch expects the fourth batch already installed; missing: " + ", ".join(missing))
PY

# Exact before/after accounting. The pre-run also catches stale/malformed local review-base changes.
VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh
cp "$WORK_DIR/source-family-coverage.json" "$PRE_COVERAGE"

python3 tools/vegeta/apply-vegeta-s4-fifth-batch.py \
  --review-base "$REVIEW_BASE" \
  --workspace-decisions "$WORKSPACE_DECISIONS" \
  --family-extension "$FIFTH_EXTENSION" \
  --fifth-batch-decisions "$FIFTH_BATCH" \
  --review-evidence "$FIFTH_REVIEW_EVIDENCE" \
  --generated-evidence "$GENERATED_EVIDENCE" \
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
  --batch-label "fifth human-reviewed projected conflict-closure batch" \
  --target-conflict "$MIN_CONFLICT" \
  --target-storage-access "$DIAG_STORAGE_ACCESS_REFERENCE" \
  --output "$DELTA_JSON" \
  --text-output "$DELTA_TXT"

echo
echo "PASS: reviewed fifth-batch subset applied and exact scheduler-fidelity family coverage recomputed"
echo "delta:     $DELTA_TXT"
echo "coverage:  $WORK_DIR/source-family-coverage.txt"
echo "gate:      $WORK_DIR/family-review-readiness.txt"
echo "closure:   $WORK_DIR/conflict-closure.txt"
echo
if grep -q '^family freeze gate: PASS' "$WORK_DIR/conflict-closure.txt"; then
  echo "Scheduler-fidelity family freeze gate is closed. Freeze only after inspecting the reviewed evidence and exact delta."
elif grep -q '^conflict pairs: .*target=95.00%' "$WORK_DIR/conflict-closure.txt" && grep -q '^remaining unique conflict pairs to target: 0$' "$WORK_DIR/conflict-closure.txt"; then
  echo "Conflict gate is closed; no storage-volume expansion is required for scheduler-fidelity. The access-heavy tail remains diagnostic / semantic-replay follow-up."
else
  echo "Conflict gate is still open; rerun tools/vegeta/run-vegeta-s4-plan-fifth-batch.sh before selecting another batch."
fi
