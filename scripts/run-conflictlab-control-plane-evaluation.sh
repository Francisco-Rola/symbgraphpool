#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-control-plane/$STAMP}"
mkdir -p "$OUT"

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

cargo build \
  --manifest-path "$ROOT/benchmarks/Cargo.toml" \
  -p acg-benchmark-conflictlab \
  --release \
  --target wasm32-unknown-unknown
cargo build \
  --manifest-path "$ROOT/runtime/Cargo.toml" \
  -p acg-benchmark-harness \
  --bin acg-benchmark \
  --release

GRIDS=(
  evaluation/conflictlab/control-plane-regression.grid.json
  evaluation/conflictlab/forced-speculation.grid.json
)

: > "$OUT/records.jsonl"
printf 'ConflictLab control-plane rerun\noutput: %s\n\n' "$OUT" > "$OUT/summary.txt"

for GRID in "${GRIDS[@]}"; do
  NAME="$(basename "$GRID" .grid.json)"
  RUN_OUT="$OUT/$NAME"
  mkdir -p "$RUN_OUT"
  echo "=== $NAME ===" | tee -a "$OUT/summary.txt"
  "$ROOT/scripts/run-conflictlab-release-matrix.sh" "$GRID" "$RUN_OUT" 2>&1 | tee "$RUN_OUT/run.log"
  cat "$RUN_OUT/records.jsonl" >> "$OUT/records.jsonl"
  python3 - "$RUN_OUT/acceptance.json" <<'PY' | tee -a "$OUT/summary.txt"
import json, sys
report=json.load(open(sys.argv[1], encoding='utf-8'))
print(f"status={report['status']} expected={report['expected_runs']} accepted={report['accepted_runs']} correctness_failures={report['correctness_failures']} configuration_errors={report['configuration_errors']}")
PY
  echo | tee -a "$OUT/summary.txt"
done

python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-control-plane.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt" | tee -a "$OUT/summary.txt"

echo "PASS: ConflictLab control-plane evaluation completed" | tee -a "$OUT/summary.txt"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/summary.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
