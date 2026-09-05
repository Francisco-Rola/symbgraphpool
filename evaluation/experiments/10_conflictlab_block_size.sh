#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"; cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
for tx in ${PAPER_EVAL_BLOCK_SIZES:-8 16 32 64 128 256 512 1024}; do
  dir="$RESULT_ROOT/10-block-size/tx-$tx"; input="$dir/inputs"; mkdir -p "$input"
  python3 evaluation/workloads/generate_conflictlab.py --output-dir "$input" --blocks 10 --transactions "$tx" --lanes "$tx" --work-iterations 131072 --tag "block-$tx"
  bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name "conflictlab-block-$tx" --execution-dir "$input" --symbolic-dir "$input/symbolic" --output "$dir" --workers "$FEATURE_WORKERS" --samples "$SAMPLES" --blocks 10 --compute-scale 0
done
