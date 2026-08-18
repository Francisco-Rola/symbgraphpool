#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 -m unittest scripts.tests.test_vegeta_workload_tools -v
python3 -m py_compile \
  scripts/vegeta/*.py \
  scripts/internal/summarize-vegeta-s3.py \
  scripts/tests/test_vegeta_workload_tools.py
bash -n scripts/run-vegeta-s3-smoke.sh
bash -n scripts/run-vegeta-s3-characterization.sh
bash -n scripts/run-vegeta-s3-native-family-dossier.sh
bash -n scripts/run-vegeta-s3-native-plan.sh
bash -n scripts/run-vegeta-s3-background-gap.sh
bash -n scripts/run-vegeta-s3-finalize-native-map.sh
bash -n scripts/run-vegeta-s3-native-translation-evaluation.sh
bash -n scripts/run-vegeta-s3-native-implementation-validation.sh

python3 scripts/vegeta/validate-native-s3-implementation.py --repo-root .

cargo test \
  --manifest-path benchmarks/Cargo.toml \
  --workspace

cargo test \
  --manifest-path Cargo.toml \
  -p acg-symbolic-json \
  native_s3_artifacts

cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-benchmark-harness \
  vegeta_eth \
  -- --nocapture
