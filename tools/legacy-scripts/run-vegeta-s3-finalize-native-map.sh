#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
CHAR_DIR="${VEGETA_S3_CHARACTERIZATION:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"
FAMILY_MAP="${VEGETA_S3_NATIVE_FAMILY_MAP:-evaluation/vegeta/s3-native-family-map.v2.json}"
GATE_CONFIG="${VEGETA_S3_NATIVE_GATE_CONFIG:-evaluation/vegeta/s3-native-preexecution-gates.v1.json}"

python3 tools/vegeta/finalize-native-s3-map.py \
  --plan-dir "$PLAN_DIR" \
  --characterization-dir "$CHAR_DIR" \
  --base-family-map "$FAMILY_MAP" \
  --gate-config "$GATE_CONFIG" \
  "$@"

printf '\nFinalization artifacts:\n'
printf '  %s/final-native-family-map.v2.json\n' "$PLAN_DIR"
printf '  %s/selector-semantic-map.json\n' "$PLAN_DIR"
printf '  %s/background-proxy-resolution.json\n' "$PLAN_DIR"
printf '  %s/background-rank1-diagnostic.txt\n' "$PLAN_DIR"
printf '  %s/final-mapping-simulation.txt\n' "$PLAN_DIR"
