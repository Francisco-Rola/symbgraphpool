#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-fix-validation/$STAMP}"

mkdir -p "$OUT"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

cat > "$OUT/environment.txt" <<EOF
started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git_revision=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)
git_status=$(git -C "$ROOT" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')
workers=6
purpose=focused validation of receipt reuse, regime fail-safe, miss-history recovery, and cost-aware throughput objective
EOF

status=0

run_step() {
  local label="$1"
  shift
  echo "=== $label ==="
  if "$@"; then
    printf '%s,0\n' "$label" >> "$OUT/step-status.csv"
  else
    local rc=$?
    printf '%s,%s\n' "$label" "$rc" >> "$OUT/step-status.csv"
    status=1
    echo "WARN: $label failed with exit code $rc; continuing to collect focused diagnostics" >&2
  fi
}

printf 'step,exit_code\n' > "$OUT/step-status.csv"

run_step 'build ConflictLab Wasm' \
  cargo build \
    --manifest-path "$ROOT/benchmarks/Cargo.toml" \
    -p acg-benchmark-conflictlab \
    --release \
    --target wasm32-unknown-unknown

run_matrix() {
  local grid="$1"
  local name="$2"
  run_step "$name" "$ROOT/tools/internal/run-conflictlab-release-matrix.sh" "$grid" "$OUT/$name"
}

run_matrix "$ROOT/evaluation/conflictlab/fix-reorder-readset.grid.json" reorder
run_matrix "$ROOT/evaluation/conflictlab/fix-regime-failsafe.grid.json" regime
run_matrix "$ROOT/evaluation/conflictlab/fix-miss-recovery.grid.json" miss
run_matrix "$ROOT/evaluation/conflictlab/fix-cost-throughput.grid.json" cost

echo '=== summarize focused validation ==='
if python3 "$ROOT/tools/internal/summarize-conflictlab-fixes.py" \
  "$OUT" --output "$OUT/fix-validation-report.txt"; then
  printf 'summary,0\n' >> "$OUT/step-status.csv"
else
  rc=$?
  printf 'summary,%s\n' "$rc" >> "$OUT/step-status.csv"
  status=1
fi

echo
if [[ "$status" -eq 0 ]]; then
  echo 'PASS: ConflictLab focused four-fix validation completed'
else
  echo 'FAIL: ConflictLab focused four-fix validation found issues; report was still written' >&2
fi
echo "report: $OUT/fix-validation-report.txt"
echo "status: $OUT/step-status.csv"
echo "raw:    $OUT/{reorder,regime,miss,cost}/records.jsonl"

exit "$status"
