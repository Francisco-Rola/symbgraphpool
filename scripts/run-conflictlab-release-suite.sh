#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-quick}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${2:-$ROOT/benchmark-results/conflictlab-release-suite/$STAMP-$PROFILE}"
mkdir -p "$OUT"

case "$PROFILE" in
  quick)
    GRIDS=(evaluation/conflictlab/quick.grid.json)
    ;;
  core)
    GRIDS=(
      evaluation/conflictlab/granularity.grid.json
      evaluation/conflictlab/contention.grid.json
      evaluation/conflictlab/block-scaling.grid.json
      evaluation/conflictlab/phase-change.grid.json
    )
    ;;
  full)
    GRIDS=(
      evaluation/conflictlab/granularity.grid.json
      evaluation/conflictlab/contention.grid.json
      evaluation/conflictlab/block-scaling.grid.json
      evaluation/conflictlab/phase-change.grid.json
      evaluation/conflictlab/ingress-block.grid.json
      evaluation/conflictlab/policy-tuning.grid.json
    )
    ;;
  *)
    echo "usage: $0 [quick|core|full] [output-dir]" >&2
    exit 2
    ;;
esac

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

# The suite is intentionally release-only. Build the real Wasm contract and runner before timing.
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

: > "$OUT/records.jsonl"
printf 'ConflictLab release suite\nprofile: %s\noutput: %s\n\n' "$PROFILE" "$OUT" > "$OUT/summary.txt"

for GRID in "${GRIDS[@]}"; do
  NAME="$(basename "$GRID" .grid.json)"
  RUN_OUT="$OUT/$NAME"
  mkdir -p "$RUN_OUT"
  echo "=== $NAME ===" | tee -a "$OUT/summary.txt"
  "$ROOT/scripts/run-conflictlab-release-matrix.sh" "$GRID" "$RUN_OUT" 2>&1 | tee "$RUN_OUT/run.log"
  cat "$RUN_OUT/records.jsonl" >> "$OUT/records.jsonl"
  python3 - "$RUN_OUT/acceptance.json" <<'PY' >> "$OUT/summary.txt"
import json, sys
report=json.load(open(sys.argv[1], encoding='utf-8'))
print(f"status={report['status']} expected={report['expected_runs']} accepted={report['accepted_runs']} correctness_failures={report['correctness_failures']}")
PY
  echo >> "$OUT/summary.txt"
done

python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"

echo "PASS: ConflictLab $PROFILE release suite completed" | tee -a "$OUT/summary.txt"
echo "combined records: $OUT/records.jsonl"
echo "plot-ready: $OUT/aggregate/plot-long.csv"
