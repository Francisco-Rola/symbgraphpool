#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
OUTPUT_DIR="${VEGETA_S3_CHARACTERIZATION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"

if [[ ! -f "$CORPUS" ]]; then
  echo "missing Vegeta S3 corpus: $CORPUS" >&2
  echo "extract it first with tools/vegeta/extract-vegeta-ethereum.py" >&2
  exit 2
fi

exec python3 tools/vegeta/characterize-vegeta-corpus.py \
  "$CORPUS" \
  --output-dir "$OUTPUT_DIR" \
  "$@"
