#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

results_root="${ACG_RESULTS_ROOT:-benchmark-results}"
stamp="$(date -u +%Y%m%d-%H%M%S)"
out_dir="$results_root/brick5f-acceptance/$stamp"
mkdir -p "$out_dir"
summary="$out_dir/summary.txt"

run_test() {
  local label="$1"
  shift
  local log="$out_dir/${label}.log"
  echo "=== $label ===" | tee -a "$summary"
  set +e
  "$@" 2>&1 | tee "$log"
  local status=${PIPESTATUS[0]}
  set -e
  grep -E '^test result:' "$log" | tee -a "$summary" || true
  echo | tee -a "$summary"
  if [[ $status -ne 0 ]]; then
    echo "FAILED: $label" >&2
    tail -80 "$log" >&2 || true
    exit "$status"
  fi
}

run_test acceptance \
  cargo test --manifest-path runtime/Cargo.toml -p acg-evaluation --test acceptance -- --nocapture
run_test metadata-capture \
  cargo test --manifest-path runtime/Cargo.toml -p acg-evaluation --test metadata_capture -- --nocapture
run_test experiment-schema \
  cargo test --manifest-path runtime/Cargo.toml -p acg-evaluation --test schema -- --nocapture
run_test runtime-measurement-record \
  cargo test --manifest-path runtime/Cargo.toml -p acg-evaluation --test brick5e_runtime -- --nocapture
run_test evaluation-lib \
  cargo test --manifest-path runtime/Cargo.toml -p acg-evaluation --lib -- --nocapture

cat <<EOF2 | tee -a "$summary"
PASS: Brick 5F acceptance gates completed.
Generic validator:
  ./scripts/validate-experiment-records.sh <manifest.json> <records.jsonl> [acceptance-report.json]
Example manifest:
  evaluation/example-manifest.json
EOF2

echo "results: $out_dir"
