#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-phase5/$STAMP}"
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

CONTROL_GRID="evaluation/conflictlab/phase5-control-plane.grid.json"
MIXED_GRID="evaluation/conflictlab/phase5-mixed-admission.grid.json"

"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$CONTROL_GRID" "$OUT/control-plane" 2>&1 | tee "$OUT/control-plane-run.log"
"$ROOT/scripts/run-conflictlab-release-matrix.sh" "$MIXED_GRID" "$OUT/mixed-admission" 2>&1 | tee "$OUT/mixed-admission-run.log"

cat \
  "$OUT/control-plane/records.jsonl" \
  "$OUT/mixed-admission/records.jsonl" \
  > "$OUT/records.jsonl"

cp "$OUT/control-plane/acceptance.json" "$OUT/acceptance-control-plane.json"
cp "$OUT/mixed-admission/acceptance.json" "$OUT/acceptance-mixed-admission.json"

VALIDATE=(python3 "$ROOT/scripts/validate-conflictlab-phase5.py" "$OUT/records.jsonl")
if [[ -n "${PHASE4_RECORDS:-}" ]]; then
  VALIDATE+=(--baseline "$PHASE4_RECORDS")
fi
"${VALIDATE[@]}" | tee "$OUT/validation.txt"

python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-phase5.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

echo "PASS: Phase 5 ConflictLab evaluation completed (432 control-plane + 48 mixed-admission = 480 runs)"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/validation.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
