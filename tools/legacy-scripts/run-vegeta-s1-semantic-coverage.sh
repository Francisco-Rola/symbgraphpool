#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
MIN_CONFLICT="${VEGETA_S1_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_MEDIAN_BLOCK="${VEGETA_S1_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}"
MIN_SEM_TX="${VEGETA_S1_MIN_SEMANTIC_TX_COVERAGE:-0.80}"
ALLOW_LOW="${VEGETA_S1_ALLOW_LOW_COVERAGE:-0}"

[[ -s "$CORPUS" ]] || { echo "missing Vegeta S1 corpus: $CORPUS" >&2; exit 2; }
[[ -s "$WORK_DIR/code-cache.json" ]] || { echo "missing completed historical code cache: $WORK_DIR/code-cache.json" >&2; exit 2; }
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing completed callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

# Recompute owner-level coverage from the already-frozen S1 source/call/code artifacts using the
# reviewed v2 family map. This is intentionally local-only: no 5,000-block tracing is repeated.
VEGETA_S1_REUSE_CACHED_COVERAGE_INPUTS=1 \
  bash tools/legacy-scripts/run-vegeta-s1-native-coverage.sh

python3 - "$WORK_DIR/source-family-coverage.json" "$MIN_CONFLICT" "$MIN_MEDIAN_BLOCK" "$ALLOW_LOW" <<'PY'
import json,sys
p,min_conf,min_med,allow=sys.argv[1],float(sys.argv[2]),float(sys.argv[3]),sys.argv[4]=='1'
d=json.load(open(p)); conf=float(d['source_conflict_coverage']['coverage']); med=float(d['block_balanced_conflict_coverage']['median_coverage'])
print(f"S1 owner-level eligibility: aggregate={conf:.4f} (min {min_conf:.2f}) median-block={med:.4f} (min {min_med:.2f})")
if (conf < min_conf or med < min_med) and not allow:
    raise SystemExit('S1 owner-level coverage remains below the preliminary gate; expand reviewed families before semantic planning.')
PY

python3 tools/vegeta/build-native-s1-plan.py \
  --thin-corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$FAMILY_MAP" \
  --source-coverage "$WORK_DIR/source-family-coverage.json" \
  --output-dir "$PLAN_DIR"

python3 tools/vegeta/audit-vegeta-semantic-conflict-coverage.py \
  --corpus "$CORPUS" \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --source-coverage "$WORK_DIR/source-family-coverage.json" \
  --output "$PLAN_DIR/semantic-conflict-coverage.json" \
  --text-output "$PLAN_DIR/semantic-conflict-coverage.txt"

# Always emit the transaction-deficit diagnostic before enforcing the conflict gate. This keeps the
# frozen all-transaction publication threshold intact while showing whether missing semantics are
# concentrated in source state/conflict participants or in non-contention background traffic.
python3 tools/vegeta/analyze-vegeta-s1-transaction-deficit.py \
  --corpus "$CORPUS" \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --target-coverage "$MIN_SEM_TX" \
  --output "$PLAN_DIR/transaction-deficit.json" \
  --text-output "$PLAN_DIR/transaction-deficit.txt"

python3 - "$PLAN_DIR/semantic-conflict-coverage.json" "$MIN_CONFLICT" "$MIN_MEDIAN_BLOCK" "$ALLOW_LOW" <<'PY'
import json,sys
p,min_conf,min_med,allow=sys.argv[1],float(sys.argv[2]),float(sys.argv[3]),sys.argv[4]=='1'
d=json.load(open(p)); conf=float(d['coverage']); med=float(d['block_balanced']['median_coverage'] or 0)
print(f"S1 reviewed state-touch semantic gate: aggregate={conf:.4f} (min {min_conf:.2f}) median-block={med:.4f} (min {min_med:.2f})")
if (conf < min_conf or med < min_med) and not allow:
    raise SystemExit('S1 reviewed state-touch conflict coverage is below the publication gate. Review remaining opaque selectors/families; do not lower the gate for publication runs.')
PY

READINESS_ARGS=(
  --translation-coverage "$PLAN_DIR/translation-coverage.json"
  --semantic-conflict-coverage "$PLAN_DIR/semantic-conflict-coverage.json"
  --transaction-deficit "$PLAN_DIR/transaction-deficit.json"
  --profile scheduler-fidelity
  --min-conflict "$MIN_CONFLICT"
  --min-median-block "$MIN_MEDIAN_BLOCK"
  --min-semantic-tx "$MIN_SEM_TX"
  --min-contention-tx "${VEGETA_S1_MIN_CONTENTION_TX_COVERAGE:-0.80}"
  --output "$PLAN_DIR/readiness.json"
  --text-output "$PLAN_DIR/readiness.txt"
  --allow-low
)
python3 tools/vegeta/evaluate-vegeta-s1-readiness.py "${READINESS_ARGS[@]}"

echo
echo "PASS: Vegeta S1 reviewed state-semantics coverage gate passed"
echo "owner-level coverage: $WORK_DIR/source-family-coverage.txt"
echo "semantic coverage: $PLAN_DIR/semantic-conflict-coverage.txt"
echo "translation coverage: $PLAN_DIR/translation-coverage.txt"
echo "transaction deficit: $PLAN_DIR/transaction-deficit.txt"
echo "readiness profiles: $PLAN_DIR/readiness.txt"
