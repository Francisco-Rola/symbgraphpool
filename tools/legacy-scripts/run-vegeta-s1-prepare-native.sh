#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
EXEC_DIR="${VEGETA_S1_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
MINT_SEQUENCE="${VEGETA_S1_CW721_MINT_SEQUENCE:-$WORK_DIR/cw721-drop-mint-sequence.json}"
S3_CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
S3_PLAN="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}/native-plan.jsonl"
MIN_CONFLICT="${VEGETA_S1_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_MEDIAN_BLOCK="${VEGETA_S1_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}"
MIN_SEM_TX="${VEGETA_S1_MIN_SEMANTIC_TX_COVERAGE:-0.80}"
MIN_SEM_FRAME="${VEGETA_S1_MIN_SEMANTIC_FRAME_COVERAGE:-0.00}"
ALLOW_LOW="${VEGETA_S1_ALLOW_LOW_COVERAGE:-0}"

# First refresh the reviewed owner map, build the native plan, and enforce the stronger selector-aware
# conflict gate. This stage is local-only when the expensive S1 code/callTracer caches already exist.
VEGETA_S1_MIN_CONFLICT_COVERAGE="$MIN_CONFLICT" \
VEGETA_S1_MIN_MEDIAN_BLOCK_COVERAGE="$MIN_MEDIAN_BLOCK" \
VEGETA_S1_ALLOW_LOW_COVERAGE="$ALLOW_LOW" \
bash tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh

python3 - "$PLAN_DIR/translation-coverage.json" "$MIN_SEM_TX" "$MIN_SEM_FRAME" "$ALLOW_LOW" <<'PY'
import json,sys
p, min_tx, min_frame, allow = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4] == '1'
d=json.load(open(p))
tx=float(d['transaction_semantic_coverage']['semantic_transaction_coverage'])
fr=float(d['calls']['reviewed_state_touch_frame_coverage'])
ready=bool(d['implementation_readiness']['native_execution_ready'])
print(f"S1 semantic gate: successful-state tx={tx:.4f} (min {min_tx:.2f}) reviewed-state-touch frame={fr:.4f} (diagnostic min {min_frame:.2f}) implementation_ready={ready}")
if not ready:
    raise SystemExit('reviewed native contract/symbolic implementation files are missing')
if (tx < min_tx or fr < min_frame) and not allow:
    raise SystemExit("S1 reviewed semantic coverage is below the frozen publication-style gate. Expand/review the family map before the full native campaign, or set VEGETA_S1_ALLOW_LOW_COVERAGE=1 for diagnostics only.")
PY

if [[ -s "$S3_CORPUS" && -s "$S3_PLAN" ]]; then
  PREFIX_ARGS=(--s1-corpus "$CORPUS" --s3-corpus "$S3_CORPUS" --s1-native-plan "$PLAN_DIR/native-plan.jsonl" --s3-native-plan "$S3_PLAN" --prefix-blocks 101)
  if [[ "${VEGETA_S1_STRICT_NATIVE_PREFIX:-0}" == "1" ]]; then PREFIX_ARGS+=(--strict-native-plan); fi
  python3 tools/vegeta/validate-vegeta-s1-prefix.py "${PREFIX_ARGS[@]}"
fi

# Before any Wasm build or historical state reconstruction, freeze the narrow public ERC721 mint
# event audit used to align state-derived sequential token IDs. This is ~20 chunked eth_getLogs calls
# at the default 250-block chunk size and is reusable on subsequent preparation runs.
if [[ ! -s "$MINT_SEQUENCE" || "${VEGETA_S1_REFRESH_CW721_MINT_SEQUENCE:-0}" == "1" ]]; then
  [[ -n "${ETH_RPC_URL:-}" ]] || { echo "S1 cw721-drop mint-sequence audit is missing. Set ETH_RPC_URL once to collect public ERC721 mint logs: $MINT_SEQUENCE" >&2; exit 2; }
  VEGETA_S1_CW721_MINT_SEQUENCE="$MINT_SEQUENCE" \
    bash tools/legacy-scripts/run-vegeta-s1-cw721-mint-audit.sh
else
  echo "reusing S1 cw721-drop mint sequence: $MINT_SEQUENCE"
fi

# Validate/build the reusable base families plus the reviewed S1 drop/STG extensions.
bash tools/legacy-scripts/run-vegeta-s1-native-implementation-validation.sh "$PLAN_DIR"

INITIAL_STATE_MODE="${VEGETA_S1_NATIVE_INITIAL_STATE_MODE:-rpc}"
if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "S1 publication replay needs predecessor-block logical state. Set ETH_RPC_URL (archive-capable) or reuse a populated $EXEC_DIR/evm-initial-state-cache.json." >&2
  exit 2
fi
PREP=(
  --plan "$PLAN_DIR/native-plan.jsonl"
  --selector-map "$PLAN_DIR/selector-semantic-map.json"
  --code-cache "$WORK_DIR/code-cache.json"
  --output-dir "$EXEC_DIR"
  --initial-state-mode "$INITIAL_STATE_MODE"
  --caller-mode exact
  --dataset-label vegeta-s1-native
  --implementation-manifest evaluation/vegeta/s1-native-implementation-manifest.v1.json
  --cw721-drop-mint-sequence "$MINT_SEQUENCE"
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP+=(--rpc-url "$ETH_RPC_URL"); fi
python3 tools/vegeta/prepare-native-s3-execution.py "${PREP[@]}"
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

echo
echo "PASS: Vegeta S1 prepared Wasmd workload is ready for the five-system campaign"
echo "plan: $PLAN_DIR/native-plan.jsonl"
echo "execution: $EXEC_DIR/execution-plan.jsonl"
echo "symbolic profiles: benchmarks/symbolic/native-s3 (base registry + reviewed S1 cw721-drop/STG extensions)"
