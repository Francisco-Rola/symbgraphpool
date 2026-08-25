#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 -m py_compile \
  scripts/vegeta/evaluate-native-s3-archetype-freeze.py \
  scripts/vegeta/summarize-native-s3-scheduler.py \
  scripts/vegeta/validate-native-s3-scheduler-results.py \
  scripts/vegeta/finalize-native-s3-map.py

PYTHONPATH=scripts/vegeta python3 -m unittest \
  scripts.tests.test_vegeta_native_s3_scheduler \
  scripts.tests.test_vegeta_s3_exact_family_extensions \
  scripts.tests.test_vegeta_s3_exact_followup \
  scripts.tests.test_vegeta_workload_tools

python3 scripts/vegeta/validate-native-s3-implementation.py

bash -n scripts/run-vegeta-s3-archetype-freeze.sh
bash -n scripts/run-vegeta-s3-native-scheduler-evaluation.sh
bash -n scripts/validate-vegeta-s3-native-scheduler.sh

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
