#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
for grid in prediction-granularity prediction-recovery; do
  src="$ROOT/evaluation/grids/conflictlab/$grid.grid.json"; tmp="$RESULT_ROOT/07-prediction/$grid.grid.json"
  python3 "$ROOT/evaluation/lib/materialize_conflictlab_grid.py" "$src" "$tmp" --workers "$FEATURE_WORKERS" --profile "$PROFILE"
  run_conflictlab_grid "$tmp" "$RESULT_ROOT/07-prediction/$grid"
done
