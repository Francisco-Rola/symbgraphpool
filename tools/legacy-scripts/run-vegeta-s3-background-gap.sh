#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
CHAR_DIR="${VEGETA_S3_CHARACTERIZATION:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"

python3 tools/vegeta/build-native-background-gap.py \
  --plan-dir "$PLAN_DIR" \
  --characterization-dir "$CHAR_DIR" \
  "$@"
