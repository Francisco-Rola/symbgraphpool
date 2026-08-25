#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 -m py_compile \
  scripts/vegeta/build-native-s3-plan.py \
  scripts/vegeta/finalize-native-s3-map.py \
  scripts/vegeta/prepare-native-s3-execution.py \
  scripts/vegeta/validate-native-s3-plan.py \
  scripts/vegeta/evaluate-vegeta-s3-exact-followup.py \
  scripts/vegeta/validate-vegeta-s3-exact-family-extension-results.py

PYTHONPATH=scripts/vegeta python3 -m unittest \
  scripts.tests.test_vegeta_s3_exact_followup \
  scripts.tests.test_vegeta_s3_exact_family_extensions \
  scripts.tests.test_vegeta_workload_tools

python3 scripts/vegeta/validate-native-s3-implementation.py

if command -v cargo >/dev/null 2>&1; then
  cargo test --manifest-path benchmarks/Cargo.toml \
    -p acg-benchmark-native-s3-marketplace-router \
    -p acg-benchmark-native-s3-wrapped-native-token
else
  echo "NOTE: cargo not found; Rust contract tests skipped in this environment" >&2
fi

echo "PASS: exact-ground-truth family extension static/provenance/unit validation completed"
