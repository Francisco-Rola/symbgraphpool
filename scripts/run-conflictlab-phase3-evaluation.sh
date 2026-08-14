#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-phase3/$STAMP}"
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

CORE_GRID="evaluation/conflictlab/phase3-system.grid.json"
EXPLORE_GRID="evaluation/conflictlab/phase3-exploration.grid.json"

"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$CORE_GRID" "$OUT/system" 2>&1 | tee "$OUT/system-run.log"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$EXPLORE_GRID" "$OUT/exploration" 2>&1 | tee "$OUT/exploration-run.log"

cat "$OUT/system/records.jsonl" "$OUT/exploration/records.jsonl" > "$OUT/records.jsonl"
cp "$OUT/system/acceptance.json" "$OUT/acceptance-system.json"
cp "$OUT/exploration/acceptance.json" "$OUT/acceptance-exploration.json"
python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-phase3.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

echo "PASS: Phase 3 ConflictLab evaluation completed (864 system + 108 exploration = 972 runs)"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
