#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# Local-only review loop. Rebuild the plan from frozen code/call caches and emit exact conflict-pair
# gain for every remaining mapped-owner opaque selector. Low coverage is expected while planning.
VEGETA_S1_ALLOW_LOW_COVERAGE=1 bash tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh

echo
echo "Opaque-selector exact-gain plan:"
echo "  benchmarks/corpora/vegeta-ethereum/s1/native-plan/semantic-conflict-coverage.txt"
echo "Machine-readable top-200 + two-selector synergies:"
echo "  benchmarks/corpora/vegeta-ethereum/s1/native-plan/semantic-conflict-coverage.json"
