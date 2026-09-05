#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/native-execution"
require_dir "$EXEC"
bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name vegeta-s3-wasmd --execution-dir "$EXEC" --symbolic-dir "$ROOT/benchmarks/symbolic/native-s3" --output "$RESULT_ROOT/03-s3-breakdown" --workers "$WORKERS" --samples "$SAMPLES" --blocks 101 --compute-scale 4 --exact-oracle 1 --allowed-missing-source 2 --stream-plan 0
