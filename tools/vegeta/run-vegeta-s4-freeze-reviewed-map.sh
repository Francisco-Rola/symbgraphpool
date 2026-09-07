#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

if [[ "${VEGETA_S4_REVIEW_ACK:-0}" != "1" ]]; then
  cat >&2 <<'MSG'
Refusing to freeze S4 review decisions without explicit human attestation.
After reviewing the edited decision file and mapping bases, rerun with:
  VEGETA_S4_REVIEW_ACK=1 bash tools/vegeta/run-vegeta-s4-freeze-reviewed-map.sh
MSG
  exit 2
fi

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
DRAFT_MAP="${VEGETA_S4_REVIEW_DRAFT_MAP:-$WORK_DIR/s4-native-family-map.reviewed-draft.json}"
OUTPUT="${VEGETA_S4_NATIVE_FAMILY_MAP:-evaluation/vegeta/s4-native-family-map.v1.json}"

# Strict recomputation: this exits non-zero until conflict, median-block, and gas gates pass.
bash tools/vegeta/run-vegeta-s4-review-check.sh

python3 tools/vegeta/freeze-vegeta-s4-family-map.py \
  --draft-map "$DRAFT_MAP" \
  --freeze-readiness "$WORK_DIR/family-review-readiness.json" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --provenance "$WORK_DIR/corpus-provenance.json" \
  --output "$OUTPUT" \
  --reviewed

echo
echo "PASS: S4 reviewed family map frozen: $OUTPUT"
echo "Next: bash evaluation/workloads/prepare_s4.sh"
