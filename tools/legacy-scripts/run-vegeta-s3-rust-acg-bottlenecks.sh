#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"; cd "$ROOT"
INPUT="${1:-${VEGETA_S3_RUST_ACG_BOTTLENECK_INPUT:-}}"
if [[ -z "$INPUT" ]]; then
  echo "usage: bash tools/legacy-scripts/run-vegeta-s3-rust-acg-bottlenecks.sh <records.jsonl>" >&2
  echo "example: ... rust-acg-risk-sweep/default/records.jsonl" >&2
  exit 2
fi
OUT="${VEGETA_S3_RUST_ACG_BOTTLENECK_DIR:-$(dirname "$INPUT")/bottlenecks}"
python3 tools/vegeta/summarize-wasmd-rust-acg-bottlenecks.py --input "$INPUT" --output-dir "$OUT" --top "${VEGETA_S3_RUST_ACG_WORST_BLOCKS:-10}"
echo "JSON: $OUT/worst-blocks.json"
