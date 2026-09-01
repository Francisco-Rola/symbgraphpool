#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
OUT="${VEGETA_S1_BLITKIN_C966_EFFECT_AUDIT:-$WORK_DIR/blitkin-c96602d9-effect-audit.json}"
TXT="${VEGETA_S1_BLITKIN_C966_EFFECT_AUDIT_TEXT:-$WORK_DIR/blitkin-c96602d9-effect-audit.txt}"

[[ -s "$PLAN_DIR/native-plan.jsonl" ]] || {
  echo "missing current S1 native plan: $PLAN_DIR/native-plan.jsonl" >&2
  echo "run tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh first" >&2
  exit 2
}
[[ -n "${ETH_RPC_URL:-}" ]] || { echo "set ETH_RPC_URL for the public-log effect audit" >&2; exit 2; }

python3 tools/vegeta/audit-vegeta-s1-erc721-selector-effects.py \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --owner 0xbd18e233e12f2a066f5b5a351285ab5a39b1f2ac \
  --selector 0xc96602d9 \
  --rpc-url "$ETH_RPC_URL" \
  --output "$OUT" \
  --text-output "$TXT"
