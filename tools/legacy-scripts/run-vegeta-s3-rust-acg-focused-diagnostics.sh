#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR="${VEGETA_S3_RUST_ACG_FOCUSED_DIR:-$EXEC_DIR/rust-acg-focused-diagnostics}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_MODE="${VEGETA_S3_RUST_ACG_FOCUSED_MODE:-debug}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS="${VEGETA_S3_RUST_ACG_FOCUSED_WORKERS:-4,6}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_SAMPLES="${VEGETA_S3_RUST_ACG_FOCUSED_SAMPLES:-3}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_POLICIES="${VEGETA_S3_RUST_ACG_FOCUSED_POLICIES:-default,thresholds-only,aggressive-no-exploration,moderate+exploration,aggressive}"
export VEGETA_S3_RUST_ACG_RISK_SWEEP_BUILD="${VEGETA_S3_RUST_ACG_FOCUSED_BUILD:-1}"

echo "Focused Rust-ACG diagnostics: workers=$VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS samples=$VEGETA_S3_RUST_ACG_RISK_SWEEP_SAMPLES"
echo "Policies: $VEGETA_S3_RUST_ACG_RISK_SWEEP_POLICIES"
echo "Output: $VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR"
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-risk-sweep.sh

echo
echo "Primary summary: $VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR/summary/risk-sweep.txt"
echo "Machine-readable metrics: $VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR/summary/risk-sweep.csv"
echo "Worst-block details: $VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR/<policy>/bottlenecks/worst-blocks.txt"
