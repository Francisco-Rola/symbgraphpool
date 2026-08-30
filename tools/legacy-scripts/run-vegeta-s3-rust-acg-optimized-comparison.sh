#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

MODE="${VEGETA_S3_RUST_ACG_COMPARISON_MODE:-debug}"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
OUT_DIR="${VEGETA_S3_RUST_ACG_COMPARISON_DIR:-$EXEC_DIR/rust-acg-optimized-comparison-$MODE}"

export VEGETA_S3_PUBLICATION_MODE="$MODE"
export VEGETA_S3_PUBLICATION_MATRIX_DIR="$OUT_DIR"
export VEGETA_S3_RUST_ACG_VISIBILITY=mvcc
export VEGETA_S3_RUST_ACG_VALIDATION=indexed
export VEGETA_S3_RUST_ACG_FEEDBACK=profile
# The Wasmd four-way rows are always enabled by the publication matrix. Keep
# the access-replay diagnostic opt-in so this comparison focuses on real Wasmd.
export VEGETA_S3_PUBLICATION_COSMOS_BLOCKSTM="${VEGETA_S3_PUBLICATION_COSMOS_BLOCKSTM:-0}"
export VEGETA_S3_PUBLICATION_STRATEGIES="${VEGETA_S3_PUBLICATION_STRATEGIES:-serial,aria-fb,vegeta,static,exact-access}"

bash tools/legacy-scripts/run-vegeta-s3-publication-matrix.sh

echo
echo "Optimized Rust-ACG comparison complete: $OUT_DIR"
echo "Human summary: $OUT_DIR/summary.txt"
echo "Machine summary: $OUT_DIR/summary.csv"
echo "Per-block phase metrics: $OUT_DIR/cosmos-wasmd-block-stm-records.jsonl"
