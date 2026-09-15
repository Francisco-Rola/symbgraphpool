#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
WORK_DIR="${VEGETA_S4_NATIVE_WORK_DIR:-$S4_DIR/native-characterization}"
PLAN_DIR="${VEGETA_S4_NATIVE_PLAN_DIR:-$S4_DIR/native-plan}"
EXEC_DIR="${VEGETA_S4_NATIVE_EXEC_DIR:-$S4_DIR/native-execution}"
FAMILY_MAP="${VEGETA_S4_NATIVE_FAMILY_MAP:-evaluation/vegeta/s4-native-family-map.v1.json}"
IMPL_MANIFEST="${VEGETA_S4_IMPLEMENTATION_MANIFEST:-evaluation/vegeta/s4-native-implementation-manifest.v1.json}"
INITIAL_STATE_MODE="${VEGETA_S4_NATIVE_INITIAL_STATE_MODE:-rpc}"

for path in \
  "$PLAN_DIR/native-plan.jsonl" \
  "$PLAN_DIR/selector-semantic-map.json" \
  "$PLAN_DIR/readiness.json" \
  "$WORK_DIR/code-cache.json" \
  "$FAMILY_MAP" \
  "$IMPL_MANIFEST"; do
  [[ -s "$path" ]] || { echo "resume input missing: $path" >&2; exit 2; }
done

if [[ ! -s "$EXEC_DIR/execution-plan.jsonl.tmp" && ! -s "$EXEC_DIR/execution-plan.jsonl" ]]; then
  echo "No completed translated execution stream is available to resume." >&2
  echo "Expected $EXEC_DIR/execution-plan.jsonl.tmp (preferred) or execution-plan.jsonl." >&2
  echo "Run the normal S4 preparation path once to finish translation." >&2
  exit 2
fi

python3 - "$PLAN_DIR/readiness.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
if d.get('dataset')!='vegeta-s4': raise SystemExit(f"unexpected readiness dataset in {p}: {d.get('dataset')!r}")
if not d.get('selected_profile_ready'): raise SystemExit(f"S4 readiness is not green in {p}; refusing execution-only resume")
print(f"resume readiness check: profile={d.get('selected_profile')} ready=yes")
PY

if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "Execution resume still needs predecessor-block state." >&2
  echo "Set ETH_RPC_URL, or provide the populated $EXEC_DIR/evm-initial-state-cache.json." >&2
  exit 2
fi

echo "S4 execution-only resume"
echo "  skipping: characterization, coverage audits, native-plan rebuild, readiness recompute, Cargo build"
echo "  reusing:  $PLAN_DIR/native-plan.jsonl"
if [[ -s "$EXEC_DIR/execution-plan.jsonl.tmp" ]]; then
  echo "  translated: $EXEC_DIR/execution-plan.jsonl.tmp"
else
  echo "  translated: $EXEC_DIR/execution-plan.jsonl"
fi
echo "  RPC cache:  $EXEC_DIR/evm-initial-state-cache.json"
echo "  RPC pacing: ${VEGETA_NATIVE_RPC_MIN_INTERVAL_SECONDS:-0.05}s minimum; retries=${VEGETA_NATIVE_RPC_MAX_RETRIES:-10}"
echo

MINT_SEQUENCE="${VEGETA_S4_CW721_MINT_SEQUENCE:-$PLAN_DIR/cw721-drop-mint-sequence.json}"
if [[ ! -s "$MINT_SEQUENCE" ]]; then
  if [[ -z "${ETH_RPC_URL:-}" ]]; then
    echo "S4 sequential CW721 mint-cardinality audit is missing: $MINT_SEQUENCE" >&2
    echo "Set ETH_RPC_URL once to collect public Transfer(from=0) logs, then later rebuilds can reuse the frozen audit." >&2
    exit 2
  fi
  echo "Collecting S4 public CW721 mint sequence for reviewed sequential-mint instances..."
  python3 tools/vegeta/collect-vegeta-cw721-drop-mints.py \
    --family-map "$FAMILY_MAP" \
    --native-plan "$PLAN_DIR/native-plan.jsonl" \
    --dataset-label vegeta-s4 \
    --rpc-url "$ETH_RPC_URL" \
    --output "$MINT_SEQUENCE"
else
  echo "Reusing S4 public CW721 mint sequence: $MINT_SEQUENCE"
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
  --resume-after-translation
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP+=(--rpc-url "$ETH_RPC_URL"); fi
PREP+=(--cw721-drop-mint-sequence "$MINT_SEQUENCE")
if [[ -n "${VEGETA_S4_ERC721_SELECTOR_MINT_AUDIT:-}" ]]; then PREP+=(--erc721-selector-mint-audit "$VEGETA_S4_ERC721_SELECTOR_MINT_AUDIT"); fi

python3 tools/vegeta/prepare-native-s3-execution.py "${PREP[@]}"

echo
echo "Validating resumed native execution bundle..."
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

echo "Materializing symbolic bundle..."
rm -rf "$EXEC_DIR/symbolic"
python3 tools/vegeta/materialize-vegeta-symbolic-bundle.py \
  --family-map "$FAMILY_MAP" \
  --output-dir "$EXEC_DIR/symbolic"

echo
echo "PASS: Vegeta S4 native Wasmd bundle prepared (execution-only resume)"
echo "readiness: $PLAN_DIR/readiness.txt"
echo "execution: $EXEC_DIR/execution-plan.jsonl"
echo "manifest: $EXEC_DIR/execution-manifest.json"
echo "symbolic: $EXEC_DIR/symbolic/"
echo
echo "Next: PAPER_EVAL_PROFILE=debug bash evaluation/experiments/02_s4_headline.sh"
