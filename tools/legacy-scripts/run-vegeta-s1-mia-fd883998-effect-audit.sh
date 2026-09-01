#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
OUT="${VEGETA_S1_MIA_EFFECT_AUDIT:-$WORK_DIR/mia-fd883998-effect-audit.json}"
TXT="${VEGETA_S1_MIA_EFFECT_AUDIT_TEXT:-$WORK_DIR/mia-fd883998-effect-audit.txt}"

[[ -s "$PLAN_DIR/native-plan.jsonl" ]] || {
  echo "missing current S1 native plan: $PLAN_DIR/native-plan.jsonl" >&2
  echo "run tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh first" >&2
  exit 2
}
[[ -n "${ETH_RPC_URL:-}" ]] || { echo "set ETH_RPC_URL for the public-log effect audit" >&2; exit 2; }

python3 tools/vegeta/audit-vegeta-s1-erc721-selector-effects.py \
  --native-plan "$PLAN_DIR/native-plan.jsonl" \
  --owner 0x885523263378d6f27a5b8c533ad3b05ab9e105b5 \
  --selector 0xfd883998 \
  --rpc-url "$ETH_RPC_URL" \
  --output "$OUT" \
  --text-output "$TXT"
