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

# Full-preexecution/intrinsic-stage view. A finite consensus window is required for
# realistic spill accounting, so also emit a configurable sensitivity sweep below.
python3 "$ROOT/scripts/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"
python3 "$ROOT/scripts/summarize-conflictlab-phase5.py" \
  "$OUT/records.jsonl" \
  --output "$OUT/results-summary.txt"

CONSENSUS_WINDOWS_MS="${CONSENSUS_WINDOWS_MS:-2 5 10 25 50}"
python3 "$ROOT/scripts/summarize-consensus-window-sweep.py" \
  "$OUT/records.jsonl" \
  --windows-ms $CONSENSUS_WINDOWS_MS \
  --output "$OUT/consensus-window-sweep.txt"
for WINDOW_MS in $CONSENSUS_WINDOWS_MS; do
  WINDOW_VALIDATE=(python3 "$ROOT/scripts/validate-conflictlab-phase5.py" "$OUT/records.jsonl" --preconsensus-window-ms "$WINDOW_MS")
  if [[ -n "${PHASE4_RECORDS:-}" ]]; then
    WINDOW_VALIDATE+=(--baseline "$PHASE4_RECORDS")
  fi
  "${WINDOW_VALIDATE[@]}" > "$OUT/validation-window-${WINDOW_MS}ms.txt"
  python3 "$ROOT/scripts/aggregate-experiment.py" \
    "$OUT/records.jsonl" \
    --out-dir "$OUT/aggregate-window-${WINDOW_MS}ms" \
    --preconsensus-window-ms "$WINDOW_MS"
  python3 "$ROOT/scripts/summarize-conflictlab-phase5.py" \
    "$OUT/records.jsonl" \
    --preconsensus-window-ms "$WINDOW_MS" \
    --output "$OUT/results-summary-window-${WINDOW_MS}ms.txt"
done

echo "PASS: Phase 5 ConflictLab evaluation completed (432 control-plane + 48 mixed-admission = 480 runs)"
echo "primary reporting: consensus-window-aware validation latency + execution-limited pipelined throughput"
echo "serial fallback is pre-execution eligible; unfinished work spills after consensus"
echo "consensus-window sensitivity (ms): $CONSENSUS_WINDOWS_MS"
echo "secondary reporting: non-overlapped total-work speedup"
echo "metric definitions: evaluation/conflictlab/phase5-consensus-metrics.md"
echo "upload:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/consensus-window-sweep.txt"
echo "  $OUT/validation.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
echo "  $OUT/results-summary-window-<N>ms.txt (for each configured consensus window)"
