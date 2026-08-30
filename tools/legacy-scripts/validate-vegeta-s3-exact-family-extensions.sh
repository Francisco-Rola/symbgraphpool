#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

python3 -m py_compile \
  tools/vegeta/build-native-s3-plan.py \
  tools/vegeta/finalize-native-s3-map.py \
  tools/vegeta/prepare-native-s3-execution.py \
  tools/vegeta/validate-native-s3-plan.py \
  tools/vegeta/evaluate-vegeta-s3-exact-followup.py \
  tools/vegeta/validate-vegeta-s3-exact-family-extension-results.py

PYTHONPATH=tools/vegeta python3 -m unittest \
  tools.tests.test_vegeta_s3_exact_followup \
  tools.tests.test_vegeta_s3_exact_family_extensions \
  tools.tests.test_vegeta_workload_tools

python3 tools/vegeta/validate-native-s3-implementation.py

if command -v cargo >/dev/null 2>&1; then
  cargo test --manifest-path benchmarks/Cargo.toml \
    -p acg-benchmark-native-s3-marketplace-router \
    -p acg-benchmark-native-s3-wrapped-native-token
else
  echo "NOTE: cargo not found; Rust contract tests skipped in this environment" >&2
fi

echo "PASS: exact-ground-truth family extension static/provenance/unit validation completed"
