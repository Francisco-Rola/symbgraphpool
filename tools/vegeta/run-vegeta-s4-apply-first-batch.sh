#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
REVIEW_BASE="${VEGETA_S4_REVIEW_BASE_MAP:-$WORK_DIR/s4-native-family-map.review-base.json}"
WORKSPACE_DECISIONS="${VEGETA_S4_REVIEW_DECISIONS:-$WORK_DIR/s4-review-decisions.draft.json}"
FAMILY_EXTENSION="${VEGETA_S4_FIRST_BATCH_FAMILY_EXTENSION:-evaluation/vegeta/s4-first-batch-native-family-extension.v1.json}"
REVIEWED_DECISIONS="${VEGETA_S4_FIRST_BATCH_REVIEWED_DECISIONS:-evaluation/vegeta/s4-first-batch-reviewed-decisions.v1.json}"

for path in "$REVIEW_BASE" "$FAMILY_EXTENSION" "$REVIEWED_DECISIONS"; do
  [[ -s "$path" ]] || { echo "missing S4 first-batch input: $path" >&2; exit 2; }
done

python3 tools/vegeta/apply-vegeta-s4-first-batch.py \
  --review-base "$REVIEW_BASE" \
  --family-extension "$FAMILY_EXTENSION" \
  --reviewed-decisions "$REVIEWED_DECISIONS" \
  --workspace-decisions "$WORKSPACE_DECISIONS"

# The first batch is an exact measurement checkpoint, not an assertion that all publication gates
# are already met. Recompute everything, but allow the readiness program to report low coverage
# without aborting this wrapper. The next queue/plan is then based on the six applied decisions.
VEGETA_S4_ALLOW_LOW_REVIEW_COVERAGE=1 bash tools/vegeta/run-vegeta-s4-review-check.sh

echo
echo "PASS: applied S4 first reviewed batch and recomputed exact coverage"
echo "decisions: $WORKSPACE_DECISIONS"
echo "coverage:  $WORK_DIR/source-family-coverage.txt"
echo "planner:   $WORK_DIR/family-marginal-coverage.txt"
echo "gate:      $WORK_DIR/family-review-readiness.txt"
echo
echo "Next: inspect the exact post-batch gate. If it is not ready, continue with the balanced all-storage-access/conflict planner; conflict-relevant access and strict gas are diagnostics."
