#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-production-scaling/$STAMP}"
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

GRID="evaluation/conflictlab/control-plane-regression.grid.json"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$GRID" "$OUT/matrix" 2>&1 | tee "$OUT/run.log"
cp "$OUT/matrix/records.jsonl" "$OUT/records.jsonl"
cp "$OUT/matrix/acceptance.json" "$OUT/acceptance.json"
cp -a "$OUT/matrix/aggregate" "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-control-plane.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

echo "PASS: production block-size scaling evaluation completed"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
