#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
PUBLICATION_MODE=false
if [[ "${1:-}" == "--publication" ]]; then
  PUBLICATION_MODE=true
  shift
fi
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [--publication] [output-directory]" >&2
  exit 2
fi
OUT="${1:-$ROOT/benchmark-results/baseline-comparison/$STAMP}"
GRID="$ROOT/evaluation/baselines/conflictlab-strategy-smoke.grid.json"

GIT_REVISION="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
GIT_STATUS_COUNT="$(git -C "$ROOT" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')"
if [[ "$PUBLICATION_MODE" == true && "$GIT_STATUS_COUNT" != "0" ]]; then
  echo "publication baseline run refused: git tree is dirty ($GIT_STATUS_COUNT status entries)" >&2
  echo "commit/stash all changes, then rerun with --publication" >&2
  exit 2
fi

mkdir -p "$OUT"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

cat > "$OUT/environment.txt" <<EOF
started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git_revision=$GIT_REVISION
git_status=$GIT_STATUS_COUNT
publication_mode=$PUBLICATION_MODE
workers=6
purpose=same-vm controlled comparison of serial AriaFB-like Vegeta-like exact-access and SymbGraphPool
EOF

echo '=== build real ConflictLab Wasm ==='
cargo build \
  --manifest-path "$ROOT/benchmarks/Cargo.toml" \
  -p acg-benchmark-conflictlab \
  --release \
  --target wasm32-unknown-unknown

echo '=== run cross-strategy smoke matrix (63 records) ==='
"$ROOT/scripts/internal/run-conflictlab-release-matrix.sh" "$GRID" "$OUT/run"

python3 "$ROOT/scripts/internal/summarize-baseline-comparison.py" \
  "$OUT/run/records.jsonl" \
  --output "$OUT/baseline-report.txt" \
  --matched-output "$OUT/baseline-matched-serial.csv"

echo
echo 'PASS: cross-strategy baseline smoke completed'
echo 'artifacts:'
echo "  $OUT/baseline-report.txt"
echo "  $OUT/baseline-matched-serial.csv"
echo "  $OUT/run/records.jsonl"
echo "  $OUT/run/aggregate/plot-long.csv"
echo "  $OUT/environment.txt"
