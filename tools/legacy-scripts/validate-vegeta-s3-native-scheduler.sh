#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

python3 -m py_compile \
  tools/vegeta/evaluate-native-s3-archetype-freeze.py \
  tools/vegeta/summarize-native-s3-scheduler.py \
  tools/vegeta/validate-native-s3-scheduler-results.py \
  tools/vegeta/finalize-native-s3-map.py

PYTHONPATH=tools/vegeta python3 -m unittest \
  tools.tests.test_vegeta_native_s3_scheduler \
  tools.tests.test_vegeta_s3_exact_family_extensions \
  tools.tests.test_vegeta_s3_exact_followup \
  tools.tests.test_vegeta_workload_tools

python3 tools/vegeta/validate-native-s3-implementation.py

bash -n tools/legacy-scripts/run-vegeta-s3-archetype-freeze.sh
bash -n tools/legacy-scripts/run-vegeta-s3-native-scheduler-evaluation.sh
bash -n tools/legacy-scripts/validate-vegeta-s3-native-scheduler.sh

if command -v cargo >/dev/null 2>&1; then
  cargo test --manifest-path runtime/Cargo.toml \
    -p acg-cosmwasm-engine --test bundle_execution
  cargo test --manifest-path runtime/Cargo.toml \
    -p acg-vegeta-native-s3-executor --bin acg-vegeta-native-s3-benchmark
  cargo check --manifest-path runtime/Cargo.toml \
    -p acg-cosmwasm-adapter \
    -p acg-benchmark-harness
else
  echo "NOTE: cargo not found; Rust compilation/tests skipped in this environment" >&2
fi

echo "PASS: Vegeta S3 native scheduler static/unit validation completed"
