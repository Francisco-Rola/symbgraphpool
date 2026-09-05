#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
source "$ROOT/evaluation/lib/common.sh"

SOURCE_RESULT="${PAPER_EVAL_CONSENSUS_SENSITIVITY_SOURCE:-$RESULT_ROOT/01-s1}"
RECORDS="$SOURCE_RESULT/records.jsonl"
require_file "$RECORDS"

OUT="$RESULT_ROOT/14-consensus-window-sensitivity"
SWEEP="${PAPER_EVAL_CONSENSUS_SWEEP_MS:-0,50,100,150,200,250,300,400,500,750,1000}"
mkdir -p "$OUT/summary"

# No workload execution is repeated here. The same S1 raw P/R records used by the
# headline experiment are re-summarized under external consensus-window values.
python3 "$ROOT/evaluation/wasmd/summarize.py" \
  --records "$RECORDS" \
  --output-dir "$OUT/summary" \
  --no-exact-oracle \
  --vegeta-dataset-tag S1 \
  --cost-metric gas_used \
  --consensus-windows-ms "$SWEEP"

cat > "$OUT/README.txt" <<TXT
Consensus-window sensitivity derived from: $RECORDS
Canonical paper window: ${PAPER_EVAL_CONSENSUS_WINDOW_MS:-300} ms
Sensitivity grid: $SWEEP ms
No execution was rerun; this experiment only re-evaluates overlap accounting from the original per-block P/R records.
TXT

echo "Consensus-window sensitivity: $OUT"
