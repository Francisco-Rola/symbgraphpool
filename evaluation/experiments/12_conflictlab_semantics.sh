#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
src="$ROOT/evaluation/grids/conflictlab/semantics.grid.json"; tmp="$RESULT_ROOT/12-semantics/semantics.grid.json"
python3 "$ROOT/evaluation/lib/materialize_conflictlab_grid.py" "$src" "$tmp" --workers "$FEATURE_WORKERS" --profile "$PROFILE"
run_conflictlab_grid "$tmp" "$RESULT_ROOT/12-semantics"
