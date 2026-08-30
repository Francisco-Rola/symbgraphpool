#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CHAR_DIR="${VEGETA_S3_CHARACTERIZATION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"
MAPPING="${VEGETA_S3_NATIVE_MAPPING_CANDIDATES:-$CHAR_DIR/native-family-mapping-candidates.json}"

if [[ ! -f "$MAPPING" ]]; then
  echo "missing proxy-resolved family mapping: $MAPPING" >&2
  echo "run tools/legacy-scripts/run-vegeta-s3-characterization.sh --fetch-calls --fetch-code --native-family-mapping-candidates first" >&2
  exit 2
fi

exec python3 tools/vegeta/build-native-family-dossier.py \
  "$MAPPING" \
  --output-dir "$CHAR_DIR" \
  "$@"
