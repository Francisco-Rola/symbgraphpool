#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
CHAR_DIR="${VEGETA_S3_CHARACTERIZATION:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"
OUT_DIR="${VEGETA_S3_ARCHETYPE_FREEZE_DIR:-$PLAN_DIR/archetype-freeze}"

for path in \
  "$PLAN_DIR/native-plan.jsonl" \
  "$PLAN_DIR/selector-semantic-map.json" \
  "$PLAN_DIR/translation-coverage.json" \
  "$CHAR_DIR/code-cache.json" \
  evaluation/vegeta/s3-native-preexecution-gates.v1.json \
  evaluation/vegeta/s3-native-implementation-manifest.v1.json; do
  if [[ ! -s "$path" ]]; then
    echo "missing candidate-freeze input: $path" >&2
    exit 2
  fi
done

python3 tools/vegeta/evaluate-native-s3-archetype-freeze.py \
  --plan-dir "$PLAN_DIR" \
  --characterization-dir "$CHAR_DIR" \
  --output-dir "$OUT_DIR" \
  --strict

echo "PASS: Vegeta S3 candidate-archetype freeze check completed"
