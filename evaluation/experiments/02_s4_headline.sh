#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
BLOCKS="${PAPER_EVAL_S4_BLOCKS:-$DEFAULT_S4_BLOCKS}"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s4/native-execution"
SYM="${PAPER_EVAL_S4_SYMBOLIC_DIR:-$EXEC/symbolic}"
if [[ ! -s "$EXEC/execution-manifest.json" || ! -s "$EXEC/execution-plan.jsonl" || ! -d "$SYM" ]]; then
  cat <<MSG
SKIP S4: source collection may exist, but the native S4 execution bundle is not built yet.
Expected: $EXEC/{execution-manifest.json,execution-plan.jsonl} and $SYM/
When the tracer/translation is complete, this script becomes the S4 headline experiment without other changes.
MSG
  [[ "${PAPER_EVAL_REQUIRE_S4:-0}" == 1 ]] && exit 2 || exit 0
fi
bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name vegeta-s4-wasmd --execution-dir "$EXEC" --symbolic-dir "$SYM" --vegeta-tag S4 --output "$RESULT_ROOT/02-s4" --workers "$WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale "${PAPER_EVAL_S4_COMPUTE_SCALE:-4}"
