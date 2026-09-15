#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s1/native-execution"; SYM="$ROOT/benchmarks/symbolic/native-s3"; require_dir "$EXEC"
MAXW="${PAPER_EVAL_IAVL_WORKERS:-$(max_worker "$WORKERS")}"
case "$PROFILE" in smoke) BLOCKS="${PAPER_EVAL_IAVL_BLOCKS:-50}"; ISAMPLES=1;; debug) BLOCKS="${PAPER_EVAL_IAVL_BLOCKS:-200}"; ISAMPLES=1;; paper) BLOCKS="${PAPER_EVAL_IAVL_BLOCKS:-1000}"; ISAMPLES="${PAPER_EVAL_IAVL_SAMPLES:-3}";; esac
OUT="$RESULT_ROOT/17-iavl-sensitivity"
run_variant() { local name="$1" cache="$2" sync="$3"; EVAL_WASMD_IAVL_CACHE_SIZE="$cache" EVAL_WASMD_IAVL_SYNC_PRUNING="$sync" PAPER_EVAL_RESOURCE_ACCOUNTING=0 bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name "vegeta-s1-iavl-$name" --execution-dir "$EXEC" --symbolic-dir "$SYM" --vegeta-tag S1 --output "$OUT/$name" --workers "$MAXW" --samples "$ISAMPLES" --blocks "$BLOCKS" --compute-scale "${PAPER_EVAL_S1_COMPUTE_SCALE:-4}"; }
run_variant isolated-pruning 0 1
run_variant cached-async 500000 0
printf 'variant,cache_size,sync_pruning\nisolated-pruning,0,1\ncached-async,500000,0\n' > "$OUT/variants.csv"
echo "IAVL sensitivity: $OUT"
