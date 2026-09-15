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
  [[ -s "$path" ]] || { echo "execution rebuild input missing: $path" >&2; exit 2; }
done

python3 - "$PLAN_DIR/readiness.json" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p))
if d.get('dataset')!='vegeta-s4': raise SystemExit(f"unexpected readiness dataset in {p}: {d.get('dataset')!r}")
if not d.get('selected_profile_ready'): raise SystemExit(f"S4 readiness is not green in {p}; refusing execution-only rebuild")
print(f"execution rebuild readiness check: profile={d.get('selected_profile')} ready=yes")
PY

if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "Execution rebuild needs predecessor-block state." >&2
  echo "Set ETH_RPC_URL, or keep the populated $EXEC_DIR/evm-initial-state-cache.json." >&2
  exit 2
fi

mkdir -p "$EXEC_DIR"
rm -f "$EXEC_DIR/execution-plan.jsonl.tmp"

echo "S4 execution-only rebuild"
echo "  skipping: characterization, coverage audits, native-plan rebuild, readiness recompute, Cargo build"
echo "  retranslating: $PLAN_DIR/native-plan.jsonl"
echo "  preserving old final plan until replacement succeeds: $EXEC_DIR/execution-plan.jsonl"
echo "  reusing RPC cache: $EXEC_DIR/evm-initial-state-cache.json"
echo "  reason: rebuild translated token-ID/lifecycle alignment only"
echo

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

echo
echo "Validating rebuilt native execution bundle..."
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

echo "Refreshing symbolic bundle metadata..."
rm -rf "$EXEC_DIR/symbolic"
python3 tools/vegeta/materialize-vegeta-symbolic-bundle.py \
  --family-map "$FAMILY_MAP" \
  --output-dir "$EXEC_DIR/symbolic"

echo
echo "PASS: Vegeta S4 native Wasmd bundle rebuilt (execution-only)"
echo "readiness unchanged: $PLAN_DIR/readiness.txt"
echo "execution: $EXEC_DIR/execution-plan.jsonl"
echo "manifest: $EXEC_DIR/execution-manifest.json"
echo "RPC cache retained: $EXEC_DIR/evm-initial-state-cache.json"
echo
echo "Next: PAPER_EVAL_PROFILE=smoke PAPER_EVAL_REQUIRE_S4=1 bash evaluation/experiments/02_s4_headline.sh"
