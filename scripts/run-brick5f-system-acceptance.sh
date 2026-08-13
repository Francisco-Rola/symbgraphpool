#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

results_root="${ACG_RESULTS_ROOT:-benchmark-results}"
stamp="$(date -u +%Y%m%d-%H%M%S)"
out_dir="$results_root/brick5f-system/$stamp"
mkdir -p "$out_dir"
summary="$out_dir/summary.txt"

run_gate() {
  local label="$1"
  shift
  local log="$out_dir/${label}.log"
  echo "=== $label ===" | tee -a "$summary"
  set +e
  "$@" 2>&1 | tee "$log"
  local status=${PIPESTATUS[0]}
  set -e
  echo "exit_status=$status" | tee -a "$summary"
  echo | tee -a "$summary"
  if [[ $status -ne 0 ]]; then
    echo "FAILED: $label" >&2
    tail -100 "$log" >&2 || true
    exit "$status"
  fi
}

run_gate root-format cargo fmt --all -- --check
run_gate root-tests cargo test --workspace --all-targets
run_gate root-clippy cargo clippy --workspace --all-targets -- -D warnings
run_gate runtime-format cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
run_gate runtime-tests cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
run_gate runtime-clippy cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings
run_gate benchmark-tests cargo test --manifest-path benchmarks/Cargo.toml --workspace

cat <<EOF2 | tee -a "$summary"
PASS: Brick 5F full-system acceptance completed.
This gate establishes source/test/lint readiness. Individual publication experiments must still
pass their ExperimentManifest against records.jsonl with validate-experiment-records.sh.
EOF2

echo "results: $out_dir"
