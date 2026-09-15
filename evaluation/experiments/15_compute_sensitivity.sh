#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
SCALES="${PAPER_EVAL_COMPUTE_SCALES:-0,1,2,4,8}"
MAXW="${PAPER_EVAL_COMPUTE_WORKERS:-$(max_worker "$WORKERS")}"
case "$PROFILE" in
  smoke) BLOCKS="${PAPER_EVAL_COMPUTE_BLOCKS:-50}"; CSAMPLES="${PAPER_EVAL_COMPUTE_SAMPLES:-1}" ;;
  debug) BLOCKS="${PAPER_EVAL_COMPUTE_BLOCKS:-200}"; CSAMPLES="${PAPER_EVAL_COMPUTE_SAMPLES:-1}" ;;
  paper) BLOCKS="${PAPER_EVAL_COMPUTE_BLOCKS:-1000}"; CSAMPLES="${PAPER_EVAL_COMPUTE_SAMPLES:-3}" ;;
esac
DATASETS="${PAPER_EVAL_COMPUTE_DATASETS:-s1,s4}"
OUT="$RESULT_ROOT/15-compute-sensitivity"; mkdir -p "$OUT/bin"
CAL="$OUT/bin/wasmd-calibrate"
(cd benchmarks/cosmos-wasmd-blockstm-s3 && GOTOOLCHAIN="${PAPER_EVAL_GO_TOOLCHAIN:-auto}" go build -o "$CAL" .)
ITER="$($CAL --calibrate-only | tail -1)"
[[ -n "$ITER" ]] || { echo "failed to calibrate Wasmd compute loop" >&2; exit 2; }
IFS=',' read -r -a DS <<< "$DATASETS"
IFS=',' read -r -a SS <<< "$SCALES"
for dataset in "${DS[@]}"; do
  case "$dataset" in
    s1) EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s1/native-execution"; SYM="$ROOT/benchmarks/symbolic/native-s3"; TAG=S1 ;;
    s4) EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s4/native-execution"; SYM="$EXEC/symbolic"; TAG=S4 ;;
    *) echo "unknown compute-sensitivity dataset: $dataset" >&2; exit 2 ;;
  esac
  if [[ ! -s "$EXEC/execution-plan.jsonl" || ! -d "$SYM" ]]; then
    echo "SKIP compute sensitivity $dataset: prepared bundle missing"
    [[ "$dataset" == s4 && "${PAPER_EVAL_REQUIRE_S4:-0}" == 1 ]] && exit 2 || continue
  fi
  for scale in "${SS[@]}"; do
    label="$(printf '%s' "$scale" | tr '.' 'p')"
    PAPER_EVAL_RESOURCE_ACCOUNTING=0 bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" \
      --name "vegeta-${dataset}-wasmd-compute-${scale}" --execution-dir "$EXEC" --symbolic-dir "$SYM" --vegeta-tag "$TAG" \
      --output "$OUT/$dataset/scale-$label" --workers "$MAXW" --samples "$CSAMPLES" --blocks "$BLOCKS" \
      --compute-scale "$scale" --iter-per-ns "$ITER"
  done
  python3 "$ROOT/evaluation/eurosys/summarize_compute_sensitivity.py" --root "$OUT/$dataset" --dataset "${dataset^^}" \
    --scales "$SCALES" --workers "$MAXW" --output-dir "$OUT/$dataset"
done
printf '%s\n' "$ITER" > "$OUT/iterations-per-ns.txt"
echo "compute sensitivity: $OUT"
