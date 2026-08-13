#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <matrix.grid.json> [output-dir]" >&2
  exit 2
fi
GRID="$1"
if [[ "$GRID" != /* ]]; then GRID="$ROOT/$GRID"; fi
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${2:-$ROOT/benchmark-results/conflictlab-release/$STAMP/$(basename "$GRID" .grid.json)}"
mkdir -p "$OUT"
MANIFEST="$OUT/manifest.json"
RECORDS="$OUT/records.jsonl"
ACCEPTANCE="$OUT/acceptance.json"

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

if [[ ! -s "$ACG_CONFLICTLAB_WASM" ]]; then
  cargo build \
    --manifest-path "$ROOT/benchmarks/Cargo.toml" \
    -p acg-benchmark-conflictlab \
    --release \
    --target wasm32-unknown-unknown
fi

python3 "$ROOT/scripts/generate-manifest-matrix.py" "$GRID" "$MANIFEST"

cargo run --release \
  --manifest-path "$ROOT/runtime/Cargo.toml" \
  -p acg-benchmark-harness \
  --bin acg-benchmark \
  -- \
  "$MANIFEST" "$RECORDS" "$ACCEPTANCE" "$ROOT"

python3 "$ROOT/scripts/aggregate-experiment.py" "$RECORDS" --out-dir "$OUT/aggregate"

echo "accepted matrix: $GRID"
echo "records: $RECORDS"
echo "acceptance: $ACCEPTANCE"
echo "plot-ready: $OUT/aggregate/plot-long.csv"
