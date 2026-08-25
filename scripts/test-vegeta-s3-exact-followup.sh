#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 -m py_compile \
  scripts/vegeta/evaluate-vegeta-s3-exact-followup.py \
  scripts/tests/test_vegeta_s3_exact_followup.py
bash -n scripts/run-vegeta-s3-exact-followup.sh
python3 -m unittest scripts.tests.test_vegeta_s3_exact_followup -v
