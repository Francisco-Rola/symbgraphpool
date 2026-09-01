#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
TOP_OWNERS="${VEGETA_S1_EXPANSION_TOP_OWNERS:-50}"
TARGET="${VEGETA_S1_EXPANSION_TARGET:-0.95}"

for path in \
  "$CORPUS" \
  "$WORK_DIR/source-family-coverage.json" \
  "$WORK_DIR/code-cache.json" \
  "$WORK_DIR/native-family-mapping-candidates.json" \
  "$FAMILY_MAP"; do
  [[ -s "$path" ]] || { echo "missing required S1 family-planning input: $path" >&2; exit 2; }
done
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

python3 tools/vegeta/plan-vegeta-s1-family-expansion.py \
  --corpus "$CORPUS" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$FAMILY_MAP" \
  --call-cache "$WORK_DIR/call-cache" \
  --top-owners "$TOP_OWNERS" \
  --target-coverage "$TARGET" \
  --output-json "$WORK_DIR/family-expansion-plan.json" \
  --output-md "$WORK_DIR/family-expansion-plan.md" \
  --source-addresses-output "$WORK_DIR/family-expansion-source-addresses.json"

echo
echo "PASS: Vegeta S1 family-expansion plan generated"
echo "plan: $WORK_DIR/family-expansion-plan.md"
echo "machine-readable: $WORK_DIR/family-expansion-plan.json"
echo "source-review addresses: $WORK_DIR/family-expansion-source-addresses.json"
echo
echo "Do not run S1 native preparation until reviewed families are added and the coverage gates pass."
