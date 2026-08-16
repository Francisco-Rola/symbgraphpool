#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-parallelism/$STAMP}"
GRID="evaluation/conflictlab/parallelism-ceiling.grid.json"

mkdir -p "$OUT"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

cat > "$OUT/environment.txt" <<EOF
started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git_revision=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)
git_status=$(git -C "$ROOT" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')
workers=6
purpose=controlled theoretical-vs-obtained parallelism and overhead attribution
EOF

echo '=== build real ConflictLab Wasm ==='
cargo build \
  --manifest-path "$ROOT/benchmarks/Cargo.toml" \
  -p acg-benchmark-conflictlab \
  --release \
  --target wasm32-unknown-unknown

echo '=== run controlled parallelism ceiling (120 records) ==='
"$ROOT/scripts/internal/run-conflictlab-release-matrix.sh" "$GRID" "$OUT/run"

python3 "$ROOT/scripts/internal/summarize-conflictlab-parallelism.py" \
  "$OUT/run/records.jsonl" \
  --output "$OUT/parallelism-report.txt" \
  --csv "$OUT/parallelism-summary.csv"

cat "$OUT/parallelism-report.txt"

echo
echo 'PASS: ConflictLab parallelism ceiling evaluation completed'
echo 'artifacts:'
echo "  $OUT/parallelism-report.txt"
echo "  $OUT/parallelism-summary.csv"
echo "  $OUT/run/records.jsonl"
echo "  $OUT/run/aggregate/plot-long.csv"
echo "  $OUT/environment.txt"
