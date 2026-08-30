#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <manifest.json> [output-directory]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$1"
if [[ ! -f "$manifest" ]]; then
  echo "manifest not found: $manifest" >&2
  exit 2
fi

if [[ $# -eq 2 ]]; then
  output_dir="$2"
else
  stamp="$(date -u +%Y%m%d-%H%M%S)"
  output_dir="$repo_root/benchmark-results/common-harness/$stamp"
fi
mkdir -p "$output_dir"
records="$output_dir/records.jsonl"
acceptance="$output_dir/acceptance.json"
summary="$output_dir/summary.txt"

{
  echo "=== common benchmark harness ==="
  echo "manifest: $manifest"
  echo "records: $records"
  echo "acceptance: $acceptance"
} | tee "$summary"

set +e
ACG_BUILD_PROFILE="${ACG_BUILD_PROFILE:-debug}" \
  cargo run \
    --quiet \
    --manifest-path "$repo_root/runtime/Cargo.toml" \
    -p acg-benchmark-harness \
    --bin acg-benchmark \
    -- \
    "$manifest" "$records" "$acceptance" "$repo_root" \
    2>&1 | tee -a "$summary"
status=${PIPESTATUS[0]}
set -e

if [[ $status -ne 0 ]]; then
  echo "benchmark harness failed with status $status" | tee -a "$summary" >&2
  exit "$status"
fi

echo "PASS: manifest executed and Phase 5F accepted all records" | tee -a "$summary"
