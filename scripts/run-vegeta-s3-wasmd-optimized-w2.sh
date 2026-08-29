#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export VEGETA_S3_PUBLICATION_MODE=smoke
export VEGETA_S3_PUBLICATION_WORKERS=2
export VEGETA_S3_PUBLICATION_SAMPLES="${VEGETA_S3_OPT_SAMPLES:-1}"
export VEGETA_S3_WASMD_INVESTIGATE="${VEGETA_S3_WASMD_INVESTIGATE:-1}"
export VEGETA_S3_PUBLICATION_MATRIX_DIR="${VEGETA_S3_OPT_OUTPUT_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/publication-matrix-wasmd-prepared-w2}"

export VEGETA_S3_WASMD_COMPARE_PPROF_DIR="${VEGETA_S3_WASMD_COMPARE_PPROF_DIR:-$VEGETA_S3_PUBLICATION_MATRIX_DIR/compare-pprof}"
# Avoid the legacy in-process SymbGraph-only profile when using the isolated trio.
export VEGETA_S3_WASMD_SYMBGRAPH_PPROF_DIR=""

if [[ "${VEGETA_S3_SKIP_CHECK:-0}" != "1" ]]; then
  bash scripts/check-vegeta-cosmos-wasmd-blockstm.sh
fi

echo "prepared-payload Wasmd diagnostic: workers=2 samples=$VEGETA_S3_PUBLICATION_SAMPLES"
echo "output=$VEGETA_S3_PUBLICATION_MATRIX_DIR"
exec bash scripts/run-vegeta-s3-publication-matrix.sh
