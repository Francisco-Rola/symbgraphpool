#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
src="$ROOT/evaluation/grids/conflictlab/consensus-divergence.grid.json"; tmp="$RESULT_ROOT/11-consensus/consensus.grid.json"
python3 "$ROOT/evaluation/lib/materialize_conflictlab_grid.py" "$src" "$tmp" --workers "$FEATURE_WORKERS" --profile "$PROFILE" --samples "${PAPER_EVAL_GRID_SAMPLES:-$SAMPLES}"
run_conflictlab_grid "$tmp" "$RESULT_ROOT/11-consensus"
