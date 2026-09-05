#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
BLOCKS="${PAPER_EVAL_S1_BLOCKS:-$DEFAULT_S1_BLOCKS}"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s1/native-execution"
require_dir "$EXEC"
bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name vegeta-s1-wasmd --execution-dir "$EXEC" --symbolic-dir "$ROOT/benchmarks/symbolic/native-s3" --vegeta-tag S1 --output "$RESULT_ROOT/01-s1" --workers "$WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale "${PAPER_EVAL_S1_COMPUTE_SCALE:-4}"
