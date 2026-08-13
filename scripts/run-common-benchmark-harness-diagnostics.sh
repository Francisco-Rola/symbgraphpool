#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
stamp="$(date -u +%Y%m%d-%H%M%S)"
output_dir="$repo_root/benchmark-results/common-harness-diagnostics/$stamp"
mkdir -p "$output_dir"
summary="$output_dir/summary.txt"

{
  echo "=== common benchmark harness validation ==="
  echo "runtime manifest: $repo_root/runtime/Cargo.toml"
  echo "physical-core policy in smoke manifest: 6"
} | tee "$summary"

cargo test \
  --manifest-path "$repo_root/runtime/Cargo.toml" \
  -p acg-benchmark-harness \
  --all-targets \
  -- --nocapture 2>&1 | tee -a "$summary"

"$repo_root/scripts/run-benchmark-manifest.sh" \
  "$repo_root/evaluation/conflictlab-harness-smoke.json" \
  "$output_dir/smoke" 2>&1 | tee -a "$summary"

echo "PASS: common benchmark harness tests + end-to-end smoke manifest completed" | tee -a "$summary"
