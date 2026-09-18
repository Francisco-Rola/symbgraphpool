#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
BLOCKS="${PAPER_EVAL_NATIVE_BLOCKS:-20}"; TX="${PAPER_EVAL_NATIVE_TX_PER_BLOCK:-384}"
case "$PROFILE" in
  smoke) DEFAULT_HOTNESS="0 9000" ;;
  debug) DEFAULT_HOTNESS="0 5000 9000" ;;
  paper) DEFAULT_HOTNESS="0 2500 5000 7500 9000 9900" ;;
esac
HOTNESS="${PAPER_EVAL_NATIVE_HOTNESS_BPS:-$DEFAULT_HOTNESS}"
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
run_native() { local name="$1" dir="$2" sym="$3"; bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name "$name" --execution-dir "$dir" --symbolic-dir "$sym" --output "$RESULT_ROOT/04-native/$name" --workers "$WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale 0; }
for hot in $HOTNESS; do
  dir="$RESULT_ROOT/04-native/inputs/miniwarehouse-hot${hot}"
  python3 evaluation/workloads/generate_native_apps.py miniwarehouse --output-dir "$dir" --blocks "$BLOCKS" --transactions "$TX" --hot-warehouse-bps "$hot"
  run_native "miniwarehouse-hot${hot}" "$dir" "$dir/symbolic"
  python3 evaluation/eurosys/validate_native_contention.py "$RESULT_ROOT/04-native/miniwarehouse-hot${hot}/records.jsonl"
done
dir="$RESULT_ROOT/04-native/inputs/native-mix"
python3 evaluation/workloads/generate_native_apps.py native-mix --output-dir "$dir" --blocks "$BLOCKS" --transactions "$TX"
run_native native-mix "$dir" "$ROOT/benchmarks/symbolic/native-s3"
