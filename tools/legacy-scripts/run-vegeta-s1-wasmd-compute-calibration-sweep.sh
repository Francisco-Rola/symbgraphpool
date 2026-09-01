#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S1_CALIBRATION_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
OUT_ROOT="${VEGETA_S1_CALIBRATION_DIR:-benchmark-results/wasmd-s1-compute-calibration}"
SCALES="${VEGETA_S1_CALIBRATION_SCALES:-1,2,4,8}"
WORKERS="${VEGETA_S1_CALIBRATION_WORKERS:-2}"
SAMPLES="${VEGETA_S1_CALIBRATION_SAMPLES:-1}"
MAX_BLOCKS="${VEGETA_S1_CALIBRATION_MAX_BLOCKS:-101}"
BASE_TOTAL_MS="${VEGETA_S1_CALIBRATION_BASE_TOTAL_MS:-1000}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing prepared S1 input: $p" >&2; exit 2; }
done
[[ "$MAX_BLOCKS" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_CALIBRATION_MAX_BLOCKS must be a positive integer" >&2; exit 2; }
[[ "$SAMPLES" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_CALIBRATION_SAMPLES must be a positive integer" >&2; exit 2; }

mkdir -p "$OUT_ROOT/bin"
GO_MAIN="benchmarks/cosmos-wasmd-blockstm-s3/main.go"
SMOKE_BIN="benchmark-results/wasmd-s1-smoke/bin/wasmd-scheduler-eval"
SHARED_BIN="$OUT_ROOT/bin/wasmd-scheduler-eval"

if [[ -x "$SMOKE_BIN" && ! "$GO_MAIN" -nt "$SMOKE_BIN" ]]; then
  SHARED_BIN="$SMOKE_BIN"
  echo "reusing fresh Wasmd binary from completed S1 smoke"
elif [[ -x "$SHARED_BIN" && ! "$GO_MAIN" -nt "$SHARED_BIN" ]]; then
  echo "reusing cached calibration Wasmd binary"
else
  echo "no fresh Wasmd calibration binary found; building once"
  cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
  cargo build --manifest-path runtime/Cargo.toml -p acg-wasmd-scheduler-ffi --release
  [[ -s runtime/target/release/libacg_wasmd_scheduler_ffi.a ]] || { echo "missing Rust ACG static library" >&2; exit 3; }
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && GOTOOLCHAIN="${EVAL_WASMD_GO_TOOLCHAIN:-auto}" GOFLAGS=-mod=mod go mod download)
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && CGO_ENABLED=1 GOTOOLCHAIN="${EVAL_WASMD_GO_TOOLCHAIN:-auto}" GOFLAGS=-mod=mod go build -tags acg_rust -o "$ROOT/$OUT_ROOT/bin/wasmd-scheduler-eval" .)
  SHARED_BIN="$OUT_ROOT/bin/wasmd-scheduler-eval"
fi
SHARED_BIN_ABS="$(realpath "$SHARED_BIN")"

ITER_FILE="$OUT_ROOT/iterations-per-ns.txt"
if [[ -s "$ITER_FILE" && ! "$SHARED_BIN_ABS" -nt "$ITER_FILE" ]] && grep -Eq '^[0-9]+([.][0-9]+)?$' "$ITER_FILE"; then
  ITER_PER_NS="$(cat "$ITER_FILE")"
  echo "reusing pinned compute calibration iter/ns=$ITER_PER_NS"
else
  ITER_PER_NS="$($SHARED_BIN_ABS --calibrate-only)"
  printf '%s\n' "$ITER_PER_NS" > "$ITER_FILE"
  echo "pinned compute calibration iter/ns=$ITER_PER_NS"
fi

scale_complete() {
  local dir="$1" scale="$2"
  [[ -s "$dir/records.jsonl" && -s "$dir/records.sha256" && -s "$dir/summary/summary.csv" && -s "$dir/summary/summary.txt" && -s "$dir/environment.txt" ]] || return 1
  grep -Fxq "compute_scale=$scale" "$dir/environment.txt" || return 1
  grep -Fxq "compute_base_total_ms=$BASE_TOTAL_MS" "$dir/environment.txt" || return 1
  grep -Fxq "workers=$WORKERS" "$dir/environment.txt" || return 1
  grep -Fxq "samples=$SAMPLES" "$dir/environment.txt" || return 1
  grep -Fxq "max_blocks=$MAX_BLOCKS" "$dir/environment.txt" || return 1
  grep -Fxq "isolate_strategies=1" "$dir/environment.txt" || return 1
  python3 - "$dir/summary/summary.csv" "$WORKERS" <<'PYREC'
import csv, sys
rows=list(csv.DictReader(open(sys.argv[1], encoding='utf-8')))
workers=int(sys.argv[2])
expected={
    'cosmos-wasmd-direct-serial','cosmos-wasmd-block-stm','cosmos-wasmd-aria-fb',
    'cosmos-wasmd-vegeta','cosmos-wasmd-symbgraph-rust'
}
chosen=[r for r in rows if int(r['workers']) == workers]
got={r['strategy'] for r in chosen}
serial_ok=all(str(r.get('serial_equivalent','')).lower() in {'true','1'} for r in chosen)
raise SystemExit(0 if got == expected and serial_ok else 1)
PYREC
}

IFS=',' read -r -a SCALE_LIST <<< "$SCALES"
for scale in "${SCALE_LIST[@]}"; do
  [[ "$scale" =~ ^[0-9]+([.][0-9]+)?$ ]] || { echo "invalid scale: $scale" >&2; exit 2; }
  label="scale-${scale//./p}"
  out="$OUT_ROOT/$label"
  if scale_complete "$out" "$scale"; then
    echo "PASS: reusing completed S1 calibration scale=$scale"
    continue
  fi

  mkdir -p "$out/bin"
  ln -sfn "$SHARED_BIN_ABS" "$out/bin/wasmd-scheduler-eval"
  echo
  echo "=== S1 Wasmd compute calibration scale=$scale workers=$WORKERS samples=$SAMPLES blocks=$MAX_BLOCKS ==="
  EVAL_WASMD_EXEC_DIR="$EXEC_DIR" \
  EVAL_WASMD_OUTPUT_DIR="$out" \
  EVAL_WASMD_WORKERS="$WORKERS" \
  EVAL_WASMD_SAMPLES="$SAMPLES" \
  EVAL_WASMD_MAX_BLOCKS="$MAX_BLOCKS" \
  EVAL_WASMD_COMPUTE_SCALE="$scale" \
  EVAL_WASMD_COMPUTE_BASE_TOTAL_MS="$BASE_TOTAL_MS" \
  EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS" \
  EVAL_WASMD_BUILD=0 \
  EVAL_WASMD_OVERWRITE=1 \
  EVAL_WASMD_SETUP_CHECK=0 \
  EVAL_WASMD_ISOLATE_STRATEGIES=1 \
  EVAL_WASMD_REUSE_ISOLATED_PARTS=1 \
    bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh smoke
done

python3 tools/vegeta/summarize-vegeta-s1-wasmd-compute-sweep.py \
  --root "$OUT_ROOT" \
  --scales "$SCALES" \
  --workers "$WORKERS" \
  --blocks "$MAX_BLOCKS" \
  --output-json "$OUT_ROOT/calibration-summary.json" \
  --output-text "$OUT_ROOT/calibration-summary.txt"

cat "$OUT_ROOT/calibration-summary.txt"
echo
echo "PASS: S1 Wasmd compute calibration sweep completed"
echo "resume-safe: completed scales and completed isolated strategies are cached"
