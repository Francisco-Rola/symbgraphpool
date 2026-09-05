#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
BLOCKS="${PAPER_EVAL_NATIVE_BLOCKS:-20}"; TX="${PAPER_EVAL_NATIVE_TX_PER_BLOCK:-384}"
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
run_native() { local name="$1" dir="$2" sym="$3"; bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name "$name" --execution-dir "$dir" --symbolic-dir "$sym" --output "$RESULT_ROOT/04-native/$name" --workers "$WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale 0; }
for hot in 0 9000; do
  dir="$RESULT_ROOT/04-native/inputs/miniwarehouse-hot${hot}"
  python3 evaluation/workloads/generate_native_apps.py miniwarehouse --output-dir "$dir" --blocks "$BLOCKS" --transactions "$TX" --hot-warehouse-bps "$hot"
  run_native "miniwarehouse-hot${hot}" "$dir" "$dir/symbolic"
done
dir="$RESULT_ROOT/04-native/inputs/native-mix"
python3 evaluation/workloads/generate_native_apps.py native-mix --output-dir "$dir" --blocks "$BLOCKS" --transactions "$TX"
run_native native-mix "$dir" "$ROOT/benchmarks/symbolic/native-s3"
