#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"

python3 tools/vegeta/audit-vegeta-s1-cw721-drop-translation.py \
  --plan "$PLAN_DIR/native-plan.jsonl" \
  --selector-map "$PLAN_DIR/selector-semantic-map.json" \
  --code-cache "$WORK_DIR/code-cache.json" \
  --mint-sequence "${VEGETA_S1_CW721_MINT_SEQUENCE:-$WORK_DIR/cw721-drop-mint-sequence.json}" \
  --output "$PLAN_DIR/cw721-drop-translation-audit.json" \
  --text-output "$PLAN_DIR/cw721-drop-translation-audit.txt"
