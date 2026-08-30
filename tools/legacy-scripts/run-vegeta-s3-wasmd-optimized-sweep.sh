#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

export VEGETA_S3_PUBLICATION_MODE=paper
export VEGETA_S3_PUBLICATION_WORKERS="${VEGETA_S3_OPT_WORKERS:-1,2,4,8,16}"
export VEGETA_S3_PUBLICATION_SAMPLES="${VEGETA_S3_OPT_SAMPLES:-1}"
# The serial overhead controls are intentionally off for the sweep because they
# add several extra full Wasmd executions at every worker count.
export VEGETA_S3_WASMD_INVESTIGATE="${VEGETA_S3_WASMD_INVESTIGATE:-0}"
export VEGETA_S3_PUBLICATION_MATRIX_DIR="${VEGETA_S3_OPT_OUTPUT_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/publication-matrix-wasmd-prepared-sweep}"

if [[ "${VEGETA_S3_SKIP_CHECK:-0}" != "1" ]]; then
  bash tools/legacy-scripts/check-vegeta-cosmos-wasmd-blockstm.sh
fi

echo "fingerprint-tracker Wasmd sweep: workers=$VEGETA_S3_PUBLICATION_WORKERS samples=$VEGETA_S3_PUBLICATION_SAMPLES"
echo "output=$VEGETA_S3_PUBLICATION_MATRIX_DIR"
exec bash tools/legacy-scripts/run-vegeta-s3-publication-matrix.sh
