#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
PLAN_DIR="${VEGETA_S4_NATIVE_PLAN_DIR:-$S4_DIR/native-plan}"
EXEC_DIR="${VEGETA_S4_NATIVE_EXEC_DIR:-$S4_DIR/native-execution}"
FAMILY_MAP="${VEGETA_S4_NATIVE_FAMILY_MAP:-evaluation/vegeta/s4-native-family-map.v1.json}"
IMPL_MANIFEST="${VEGETA_S4_IMPLEMENTATION_MANIFEST:-evaluation/vegeta/s4-native-implementation-manifest.v1.json}"
MIN_CONFLICT="${VEGETA_S4_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_MEDIAN="${VEGETA_S4_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}"
MIN_FAMILY_STORAGE_ACCESS="${VEGETA_S4_MIN_STORAGE_ACCESS_COVERAGE:-0.90}"
DIAG_FAMILY_CONFLICT_RELEVANT_ACCESS_REFERENCE="${VEGETA_S4_MIN_CONFLICT_RELEVANT_ACCESS_COVERAGE:-0.90}"
MIN_SEM_TX="${VEGETA_S4_MIN_SEMANTIC_TX_COVERAGE:-0.80}"
MIN_CONTENTION_TX="${VEGETA_S4_MIN_CONTENTION_TX_COVERAGE:-0.80}"
INITIAL_STATE_MODE="${VEGETA_S4_NATIVE_INITIAL_STATE_MODE:-rpc}"

for path in "$CORPUS" "$WORK_DIR/thin-corpus.jsonl" "$WORK_DIR/code-cache.json" "$WORK_DIR/corpus-provenance.json" "$FAMILY_MAP" "$IMPL_MANIFEST"; do
  [[ -s "$path" ]] || {
    echo "missing required S4 native-preparation input: $path" >&2
    if [[ "$path" == "$FAMILY_MAP" ]]; then
      echo "run tools/vegeta/run-vegeta-s4-characterize.sh, review family-review-queue.md, then freeze the reviewed map at $FAMILY_MAP" >&2
    fi
    exit 2
  }
done
[[ -d "$WORK_DIR/call-cache" ]] || { echo "missing S4 callTracer cache: $WORK_DIR/call-cache" >&2; exit 2; }

python3 - "$FAMILY_MAP" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
if d.get('dataset') != 'vegeta-s4': raise SystemExit(f"S4 family map has unexpected dataset: {d.get('dataset')!r}")
if d.get('candidate_only'): raise SystemExit('refusing candidate_only map; review/freeze evaluation/vegeta/s4-native-family-map.v1.json first')
if not d.get('freeze_evidence'): raise SystemExit('S4 family map lacks freeze_evidence; use tools/vegeta/freeze-vegeta-s4-family-map.py after the review gate passes')
PY

mkdir -p "$PLAN_DIR" "$EXEC_DIR"

# Recompute all coverage against the reviewed S4 map. No source storage keys enter the call plan.
python3 tools/vegeta/build-vegeta-delegate-resolutions.py \
  --corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --family-map "$FAMILY_MAP" \
  --output "$WORK_DIR/native-family-mapping-candidates.json"

python3 tools/vegeta/audit-vegeta-native-family-coverage.py \
  --corpus "$CORPUS" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$FAMILY_MAP" \
  --output "$WORK_DIR/source-family-coverage.json" \
  --text-output "$WORK_DIR/source-family-coverage.txt" \
  --top-unmapped "${VEGETA_S4_COVERAGE_TOP_UNMAPPED:-200}"

python3 tools/vegeta/build-native-s1-plan.py \
  --thin-corpus "$WORK_DIR/thin-corpus.jsonl" \
  --call-cache "$WORK_DIR/call-cache" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mapping-candidates "$WORK_DIR/native-family-mapping-candidates.json" \
  --family-map "$FAMILY_MAP" \
  --source-coverage "$WORK_DIR/source-family-coverage.json" \
  --output-dir "$PLAN_DIR"

python3 tools/vegeta/audit-vegeta-semantic-conflict-coverage.py \
  --dataset vegeta-s4 \
  --corpus "$CORPUS" \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --source-coverage "$WORK_DIR/source-family-coverage.json" \
  --output "$PLAN_DIR/semantic-conflict-coverage.json" \
  --text-output "$PLAN_DIR/semantic-conflict-coverage.txt"

python3 tools/vegeta/analyze-vegeta-s1-transaction-deficit.py \
  --dataset vegeta-s4 \
  --corpus "$CORPUS" \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --target-coverage "$MIN_SEM_TX" \
  --conflict-target-coverage "$MIN_CONTENTION_TX" \
  --output "$PLAN_DIR/transaction-deficit.json" \
  --text-output "$PLAN_DIR/transaction-deficit.txt"

python3 tools/vegeta/validate-vegeta-s4-readiness.py \
  --family-map "$FAMILY_MAP" \
  --family-coverage "$WORK_DIR/source-family-coverage.json" \
  --translation-coverage "$PLAN_DIR/translation-coverage.json" \
  --semantic-coverage "$PLAN_DIR/semantic-conflict-coverage.json" \
  --transaction-deficit "$PLAN_DIR/transaction-deficit.json" \
  --min-conflict "$MIN_CONFLICT" \
  --min-median-block "$MIN_MEDIAN" \
  --min-family-storage-access "$MIN_FAMILY_STORAGE_ACCESS" \
  --min-family-conflict-relevant-access "$DIAG_FAMILY_CONFLICT_RELEVANT_ACCESS_REFERENCE" \
  --min-semantic-tx "$MIN_SEM_TX" \
  --min-contention-tx "$MIN_CONTENTION_TX" \
  --output "$PLAN_DIR/readiness.json" \
  --text-output "$PLAN_DIR/readiness.txt"

# Validate/build exactly the native packages listed in the implementation manifest.
python3 tools/vegeta/validate-native-s3-implementation.py \
  --repo-root "$ROOT" --manifest "$IMPL_MANIFEST" \
  --json-output "${PLAN_DIR}/native-implementation-validation.json" \
  --text-output "${PLAN_DIR}/native-implementation-validation.txt"

if [[ "${VEGETA_S4_SKIP_NATIVE_BUILD:-0}" != "1" ]]; then
  mapfile -t packages < <(python3 - "$IMPL_MANIFEST" <<'PY'
import json,sys
for row in json.load(open(sys.argv[1])).get('families') or []:
    if row.get('package'): print(row['package'])
PY
)
  for package in "${packages[@]}"; do
    cargo test --manifest-path "$ROOT/benchmarks/Cargo.toml" -p "$package"
    cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p "$package" --release --target wasm32-unknown-unknown
  done
fi
python3 tools/vegeta/validate-native-s3-implementation.py \
  --repo-root "$ROOT" --manifest "$IMPL_MANIFEST" --require-wasm-artifacts \
  --json-output "${PLAN_DIR}/native-implementation-validation.json" \
  --text-output "${PLAN_DIR}/native-implementation-validation.txt"

if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "S4 publication preparation needs predecessor-block logical state." >&2
  echo "Set ETH_RPC_URL once, or reuse $EXEC_DIR/evm-initial-state-cache.json." >&2
  exit 2
fi
PREP=(
  --plan "$PLAN_DIR/native-plan.jsonl"
  --selector-map "$PLAN_DIR/selector-semantic-map.json"
  --code-cache "$WORK_DIR/code-cache.json"
  --implementation-manifest "$IMPL_MANIFEST"
  --output-dir "$EXEC_DIR"
  --initial-state-mode "$INITIAL_STATE_MODE"
  --caller-mode exact
  --dataset-label vegeta-s4-native
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP+=(--rpc-url "$ETH_RPC_URL"); fi
if [[ -n "${VEGETA_S4_CW721_MINT_SEQUENCE:-}" ]]; then PREP+=(--cw721-drop-mint-sequence "$VEGETA_S4_CW721_MINT_SEQUENCE"); fi
if [[ -n "${VEGETA_S4_ERC721_SELECTOR_MINT_AUDIT:-}" ]]; then PREP+=(--erc721-selector-mint-audit "$VEGETA_S4_ERC721_SELECTOR_MINT_AUDIT"); fi
python3 tools/vegeta/prepare-native-s3-execution.py "${PREP[@]}"
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

rm -rf "$EXEC_DIR/symbolic"
python3 tools/vegeta/materialize-vegeta-symbolic-bundle.py \
  --family-map "$FAMILY_MAP" \
  --output-dir "$EXEC_DIR/symbolic"

echo
echo "PASS: Vegeta S4 native Wasmd bundle prepared"
echo "readiness: $PLAN_DIR/readiness.txt"
echo "execution: $EXEC_DIR/execution-plan.jsonl"
echo "manifest: $EXEC_DIR/execution-manifest.json"
echo "symbolic: $EXEC_DIR/symbolic/"
echo
echo "Next: PAPER_EVAL_PROFILE=debug bash evaluation/experiments/02_s4_headline.sh"
