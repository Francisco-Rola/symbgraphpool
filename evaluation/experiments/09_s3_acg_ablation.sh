#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
export VEGETA_S3_RUST_ACG_ABLATION_DIR="$RESULT_ROOT/09-s3-ablation"
export VEGETA_S3_RUST_ACG_ABLATION_MODE="$PROFILE"
export VEGETA_S3_RUST_ACG_ABLATION_WORKERS="$WORKERS"
export VEGETA_S3_RUST_ACG_ABLATION_SAMPLES="$SAMPLES"
bash "$ROOT/tools/legacy-scripts/run-vegeta-s3-rust-acg-ablation.sh"
