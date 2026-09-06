#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
BASE_MAP="${VEGETA_S4_BASE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
CANDIDATE_MAP="${VEGETA_S4_CANDIDATE_FAMILY_MAP:-$WORK_DIR/s4-native-family-map.candidate.json}"
TOP="${VEGETA_S4_CHARACTERIZE_TOP:-100}"

for path in \
  "$CORPUS" \
  "$WORK_DIR/thin-corpus.jsonl" \
  "$WORK_DIR/source-summary.json" \
  "$WORK_DIR/relevant-addresses.json" \
  "$WORK_DIR/code-cache.json" \
  "$BASE_MAP"; do
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

python3 tools/vegeta/build-vegeta-delegate-resolutions.py \
  --corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --family-map "$CANDIDATE_MAP" \
  --output "$WORK_DIR/native-family-mapping-candidates.json"

python3 tools/vegeta/audit-vegeta-native-family-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$CANDIDATE_MAP" \
  --output "$WORK_DIR/source-family-coverage.json" \
  --text-output "$WORK_DIR/source-family-coverage.txt" \
  --top-unmapped "${VEGETA_S4_COVERAGE_TOP_UNMAPPED:-200}"

python3 tools/vegeta/build-vegeta-s4-review-queue.py \
  --family-summary "$WORK_DIR/family-summary.json" \
  --selector-summary "$WORK_DIR/selector-summary.json" \
  --coverage "$WORK_DIR/source-family-coverage.json" \
  --output-json "$WORK_DIR/family-review-queue.json" \
  --output-md "$WORK_DIR/family-review-queue.md" \
  --top "${VEGETA_S4_REVIEW_TOP:-50}"

echo
echo "PASS: Vegeta S4 frozen family characterization complete"
echo "family summary:       $WORK_DIR/family-summary.txt"
echo "selector summary:     $WORK_DIR/selector-summary.json"
echo "known-family coverage:$WORK_DIR/source-family-coverage.txt"
echo "semantic review queue:$WORK_DIR/family-review-queue.md"
echo "candidate family map: $CANDIDATE_MAP"
echo
echo "Next step: review the unmapped high-impact families/selectors."
echo "When satisfied, freeze a reviewed map as evaluation/vegeta/s4-native-family-map.v1.json"
echo "and run: bash tools/vegeta/run-vegeta-s4-prepare-native.sh"
