#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-phase4/$STAMP}"
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

SYSTEM_GRID="evaluation/conflictlab/phase4-system.grid.json"
VM_GRID="evaluation/conflictlab/phase4-vm-lifecycle.grid.json"
MIXED_GRID="evaluation/conflictlab/phase4-mixed.grid.json"
EXPLORE_GRID="evaluation/conflictlab/phase4-exploration.grid.json"

"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$SYSTEM_GRID" "$OUT/system" 2>&1 | tee "$OUT/system-run.log"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$VM_GRID" "$OUT/vm-lifecycle" 2>&1 | tee "$OUT/vm-lifecycle-run.log"
python3 "$ROOT/scripts/validate-phase4-vm-equivalence.py" "$OUT/vm-lifecycle/records.jsonl"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$MIXED_GRID" "$OUT/mixed" 2>&1 | tee "$OUT/mixed-run.log"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$EXPLORE_GRID" "$OUT/exploration" 2>&1 | tee "$OUT/exploration-run.log"

cat \
  "$OUT/system/records.jsonl" \
  "$OUT/vm-lifecycle/records.jsonl" \
  "$OUT/mixed/records.jsonl" \
  "$OUT/exploration/records.jsonl" \
  > "$OUT/records.jsonl"

cp "$OUT/system/acceptance.json" "$OUT/acceptance-system.json"
cp "$OUT/vm-lifecycle/acceptance.json" "$OUT/acceptance-vm-lifecycle.json"
cp "$OUT/mixed/acceptance.json" "$OUT/acceptance-mixed.json"
cp "$OUT/exploration/acceptance.json" "$OUT/acceptance-exploration.json"

python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-phase4.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

echo "PASS: Phase 4 ConflictLab evaluation completed (432 system + 36 VM lifecycle + 216 mixed + 48 exploration = 732 runs)"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
