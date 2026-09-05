#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"; TX="${PAPER_EVAL_CONTENTION_TX:-384}"; BLOCKS="${PAPER_EVAL_CONTENTION_BLOCKS:-10}"
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
for lanes in ${PAPER_EVAL_CONTENTION_LANES:-384 96 24 6 1}; do
  dir="$RESULT_ROOT/06-contention/lanes-$lanes"; input="$dir/inputs"; mkdir -p "$input"
  python3 evaluation/workloads/generate_conflictlab.py --output-dir "$input" --blocks "$BLOCKS" --transactions "$TX" --lanes "$lanes" --work-iterations 131072 --storage-rounds 3 --tag "lanes-$lanes"
  bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name "conflictlab-lanes-$lanes" --execution-dir "$input" --symbolic-dir "$input/symbolic" --output "$dir" --workers "$FEATURE_WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale 0
done
