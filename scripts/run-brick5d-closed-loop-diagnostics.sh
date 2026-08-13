#!/usr/bin/env bash
set -euo pipefail

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${ACG_BRICK5D_OUT_DIR:-benchmark-results/brick5d-closed-loop/${STAMP}}"
mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/summary.txt"
: > "$SUMMARY"

run_test() {
  local name="$1"
  shift
  local log="$OUT_DIR/${name}.log"
  echo "=== ${name} ===" | tee -a "$SUMMARY"
  set +e
  "$@" 2>&1 | tee "$log"
  local status=${PIPESTATUS[0]}
  set -e
  if (( status != 0 )); then
    echo "FAILED: ${name} (status ${status})" | tee -a "$SUMMARY" >&2
    tail -n 80 "$log" >&2
    exit "$status"
  fi
  grep -E 'test result:|Brick 5D closed loop:' "$log" | tee -a "$SUMMARY" || true
  echo | tee -a "$SUMMARY"
}

run_test feedback-cost-state \
  cargo test --release -p acg-feedback replay_cost -- --nocapture --test-threads=1

run_test candidate-cost-policy \
  cargo test --release -p acg-candidate-graph replay_cost_changes_scheduling_risk -- --nocapture --test-threads=1

run_test runtime-closed-loop \
  cargo test --release --manifest-path runtime/Cargo.toml \
    -p acg-runtime-feedback --test brick5d_closed_loop -- --nocapture --test-threads=1

run_test reconciliation-attribution \
  cargo test --release --manifest-path runtime/Cargo.toml \
    -p acg-runtime-feedback --test adaptive_pipeline \
    brick5d_reconciliation_attribution -- --nocapture --test-threads=1

run_test split-phase-reexecution-timing \
  cargo test --release --manifest-path runtime/Cargo.toml \
    -p acg-validator-sim --test speculative_parallel \
    split_phase_validator_prepares_without_commit_then_reconciles_decided_block \
    -- --nocapture --test-threads=1

echo "Logs: $OUT_DIR"
echo "Summary: $SUMMARY"
