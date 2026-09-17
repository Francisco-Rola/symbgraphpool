#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
mkdir -p "$RESULT_ROOT/eurosys-summary" "$RESULT_ROOT/paper"
python3 "$ROOT/evaluation/eurosys/summarize_headline_records.py" --dataset "S1=$RESULT_ROOT/01-s1/records.jsonl" --dataset "S4=$RESULT_ROOT/02-s4/records.jsonl" --consensus-window-ms "$CANONICAL_CONSENSUS_WINDOW_MS" --output-dir "$RESULT_ROOT/eurosys-summary"
python3 "$ROOT/evaluation/eurosys/build_tables.py" --result-root "$RESULT_ROOT" --output-dir "$RESULT_ROOT/paper"
python3 "$ROOT/evaluation/eurosys/plot_main.py" --result-root "$RESULT_ROOT" --output-dir "$RESULT_ROOT/paper"
python3 "$ROOT/evaluation/eurosys/export_latex_results.py" --result-root "$RESULT_ROOT" --output "$RESULT_ROOT/paper/evaluation-results-macros.tex"
echo "postprocessed EuroSys bundle: $RESULT_ROOT/paper"
