#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"

python3 "$ROOT/scripts/diagnose-conflictlab-v1-correctness.py" "$OUT"

echo
echo "Key files:"
echo "  $OUT/correctness-diagnostics/summary.txt"
echo "  $OUT/correctness-diagnostics/incorrect-records.csv"
echo "  $OUT/correctness-diagnostics/failure-discrimination.csv"
echo "  $OUT/correctness-diagnostics/campaign-status.csv"
echo "  $OUT/correctness-diagnostics/rerun-plan.json"
echo
echo "For exact failing-run reproduction with serial/adaptive state snapshots:"
echo "  $ROOT/scripts/rerun-conflictlab-v1-correctness-failures.sh '$OUT'"
