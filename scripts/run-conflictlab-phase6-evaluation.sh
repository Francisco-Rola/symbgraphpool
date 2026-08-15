#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-phase6/$STAMP}"
mkdir -p "$OUT"

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

echo '=== build real ConflictLab Wasm ==='
cargo build \
  --manifest-path "$ROOT/benchmarks/Cargo.toml" \
  -p acg-benchmark-conflictlab \
  --release \
  --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build \
  --manifest-path "$ROOT/runtime/Cargo.toml" \
  -p acg-benchmark-harness \
  --bin acg-benchmark \
  --release

run_grid() {
  local grid="$1"
  local name="$2"
  "$ROOT/scripts/run-conflictlab-release-matrix.sh" "$grid" "$OUT/$name" \
    2>&1 | tee "$OUT/$name-run.log"
}

run_grid evaluation/conflictlab/phase6-feature-state.grid.json feature-state
run_grid evaluation/conflictlab/phase6-consensus-cutoff.grid.json consensus-cutoff
run_grid evaluation/conflictlab/phase6-serial-preexecution-cutoff.grid.json serial-preexecution-cutoff
run_grid evaluation/conflictlab/phase6-consensus-divergence.grid.json consensus-divergence
run_grid evaluation/conflictlab/phase6-policy-sensitivity.grid.json policy-sensitivity
run_grid evaluation/conflictlab/phase6-vm-lifecycle-sanity.grid.json vm-lifecycle-sanity

cat \
  "$OUT/feature-state/records.jsonl" \
  "$OUT/consensus-cutoff/records.jsonl" \
  "$OUT/serial-preexecution-cutoff/records.jsonl" \
  "$OUT/consensus-divergence/records.jsonl" \
  "$OUT/policy-sensitivity/records.jsonl" \
  "$OUT/vm-lifecycle-sanity/records.jsonl" \
  > "$OUT/records.jsonl"

for name in feature-state consensus-cutoff serial-preexecution-cutoff consensus-divergence policy-sensitivity vm-lifecycle-sanity; do
  cp "$OUT/$name/acceptance.json" "$OUT/acceptance-$name.json"
done

python3 "$ROOT/scripts/validate-conflictlab-phase6.py" "$OUT/records.jsonl" | tee "$OUT/validation.txt"
python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-phase6.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

cat "$OUT/results-summary.txt"
echo
echo 'PASS: Phase 6 consensus-realism evaluation completed (952 real-Wasm runs)'
echo '  feature-state:                576'
echo '  consensus-cutoff:              96'
echo '  serial-preexecution-cutoff:    24'
echo '  consensus-divergence:         192'
echo '  policy-sensitivity:            48'
echo '  vm-lifecycle-sanity:            16'
echo 'primary metrics: measured post-consensus validation latency and max(pre,post) execution service time'
echo 'secondary metric: non-overlapped total-work speedup'
echo 'upload:'
echo "  $OUT/results-summary.txt"
echo "  $OUT/validation.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
