#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
OUT="${VEGETA_S1_CW721_MINT_SEQUENCE:-$WORK_DIR/cw721-drop-mint-sequence.json}"
START="${VEGETA_S1_START_BLOCK:-16774645}"
END="${VEGETA_S1_END_BLOCK:-16779644}"
CHUNK="${VEGETA_S1_MINT_LOG_CHUNK_BLOCKS:-250}"

[[ -s "$FAMILY_MAP" ]] || { echo "missing reviewed S1 family map: $FAMILY_MAP" >&2; exit 2; }
[[ -n "${ETH_RPC_URL:-}" ]] || { echo "ETH_RPC_URL is required for the narrow ERC721 mint-event audit" >&2; exit 2; }

python3 tools/vegeta/collect-vegeta-cw721-drop-mints.py \
  --family-map "$FAMILY_MAP" \
  --rpc-url "$ETH_RPC_URL" \
  --start-block "$START" \
  --end-block "$END" \
  --chunk-blocks "$CHUNK" \
  --output "$OUT"

echo
echo "PASS: reviewed S1 cw721-drop mint sequences are sequential and fit u64"
echo "mint sequence: $OUT"
