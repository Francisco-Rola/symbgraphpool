#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN="${VEGETA_S1_NATIVE_PLAN:-benchmarks/corpora/vegeta-ethereum/s1/native-plan/native-plan.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
OUT="${VEGETA_S1_MIA_MINT_AUDIT:-$WORK_DIR/mia-fd883998-mint-audit.json}"
TXT="${VEGETA_S1_MIA_MINT_AUDIT_TEXT:-$WORK_DIR/mia-fd883998-mint-audit.txt}"
START="${VEGETA_S1_START_BLOCK:-16774645}"
END="${VEGETA_S1_END_BLOCK:-16779644}"
CHUNK="${VEGETA_S1_MINT_LOG_CHUNK_BLOCKS:-250}"

[[ -s "$PLAN" ]] || { echo "missing current S1 native plan: $PLAN" >&2; exit 2; }
[[ -n "${ETH_RPC_URL:-}" ]] || { echo "ETH_RPC_URL is required for the MIA ERC721 selector audit" >&2; exit 2; }

python3 tools/vegeta/audit-vegeta-s1-erc721-selector-mints.py \
  --native-plan "$PLAN" \
  --owner 0x885523263378d6f27a5b8c533ad3b05ab9e105b5 \
  --selector 0xfd883998 \
  --rpc-url "$ETH_RPC_URL" \
  --start-block "$START" \
  --end-block "$END" \
  --chunk-blocks "$CHUNK" \
  --output "$OUT" \
  --text-output "$TXT"

echo
echo "MIA selector effect audit complete; this does not change the publication gate."
echo "Send back: $TXT"
