#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-debug}"
case "$MODE" in
  smoke|debug|paper) ;;
  *) echo "usage: bash scripts/eval-wasmd.sh [smoke|debug|paper]" >&2; exit 2 ;;
esac

physical_cores() {
  if command -v lscpu >/dev/null 2>&1; then
    local n
    n="$(lscpu -p=CORE,SOCKET 2>/dev/null | awk -F, '!/^#/ {print $1 "," $2}' | sort -u | wc -l | tr -d ' ')"
    if [[ "$n" =~ ^[0-9]+$ ]] && (( n > 0 )); then echo "$n"; return; fi
  fi
  nproc 2>/dev/null || echo 1
}

publication_workers() {
  local n="$1" v=1 out=""
  while (( v <= n )); do
    out+="${out:+,}${v}"
    v=$((v * 2))
  done
  if [[ ",$out," != *",$n,"* ]]; then out+=",$n"; fi
  echo "$out"
}

PHYSICAL_CORES="$(physical_cores)"
case "$MODE" in
  smoke)
    DEFAULT_WORKERS="2"
    DEFAULT_SAMPLES=1
    DEFAULT_REQUIRE_CLEAN=0
    ;;
  debug)
    if (( PHYSICAL_CORES >= 6 )); then DEFAULT_WORKERS="2,4,6";
    elif (( PHYSICAL_CORES >= 4 )); then DEFAULT_WORKERS="2,4";
    else DEFAULT_WORKERS="1,${PHYSICAL_CORES}"; fi
    DEFAULT_SAMPLES=1
    DEFAULT_REQUIRE_CLEAN=0
    ;;
  paper)
    DEFAULT_WORKERS="$(publication_workers "$PHYSICAL_CORES")"
    DEFAULT_SAMPLES=5
    DEFAULT_REQUIRE_CLEAN=1
    ;;
esac

WORKERS_LIST="${EVAL_WASMD_WORKERS:-$DEFAULT_WORKERS}"
SAMPLES="${EVAL_WASMD_SAMPLES:-$DEFAULT_SAMPLES}"
REQUIRE_CLEAN="${EVAL_WASMD_REQUIRE_CLEAN:-$DEFAULT_REQUIRE_CLEAN}"
BUILD="${EVAL_WASMD_BUILD:-1}"
OVERWRITE="${EVAL_WASMD_OVERWRITE:-0}"
GO_TOOLCHAIN="${EVAL_WASMD_GO_TOOLCHAIN:-auto}"
BASE_TOTAL_MS="${EVAL_WASMD_COMPUTE_BASE_TOTAL_MS:-1000}"
COMPUTE_SCALE="${EVAL_WASMD_COMPUTE_SCALE:-4}"
ALLOWED_MISSING_SOURCE="${EVAL_WASMD_ALLOWED_MISSING_SOURCE:-2}"
EXEC_DIR="${EVAL_WASMD_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${EVAL_WASMD_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
SYMBOLIC_DIR="${EVAL_WASMD_SYMBOLIC_DIR:-benchmarks/symbolic/native-s3}"
OUT_DIR="${EVAL_WASMD_OUTPUT_DIR:-benchmark-results/wasmd-${MODE}}"

MANIFEST="$EXEC_DIR/execution-manifest.json"
PLAN="$EXEC_DIR/execution-plan.jsonl"
for p in "$MANIFEST" "$PLAN"; do
  [[ -s "$p" ]] || { echo "missing evaluation input: $p" >&2; echo "prepare the frozen Vegeta S3 Wasmd translation before running this stage" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing source-trace directory: $TRACE_DIR" >&2; exit 2; }
[[ -d "$SYMBOLIC_DIR" ]] || { echo "missing symbolic profile directory: $SYMBOLIC_DIR" >&2; exit 2; }

if [[ "$REQUIRE_CLEAN" == "1" ]] && [[ -n "$(git status --porcelain)" ]]; then
  echo "paper mode requires a clean committed tree (set EVAL_WASMD_REQUIRE_CLEAN=0 only for non-publication diagnostics)" >&2
  exit 2
fi
if [[ -e "$OUT_DIR/records.jsonl" && "$OVERWRITE" != "1" ]]; then
  echo "refusing to overwrite existing campaign: $OUT_DIR (set EVAL_WASMD_OVERWRITE=1 or choose EVAL_WASMD_OUTPUT_DIR)" >&2
  exit 2
fi
mkdir -p "$OUT_DIR/bin" "$OUT_DIR/raw" "$OUT_DIR/summary"
if [[ "$OUT_DIR" = /* ]]; then OUT_DIR_ABS="$OUT_DIR"; else OUT_DIR_ABS="$ROOT/$OUT_DIR"; fi
DIRTY=no
[[ -n "$(git status --porcelain 2>/dev/null)" ]] && DIRTY=yes

ENV_FILE="$OUT_DIR/environment.txt"
{
  echo "mode=$MODE"
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "git_commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "git_dirty=$DIRTY"
  echo "physical_cores=$PHYSICAL_CORES"
  echo "logical_cpus=$(nproc 2>/dev/null || echo unknown)"
  echo "workers=$WORKERS_LIST"
  echo "samples=$SAMPLES"
  echo "compute_scale=$COMPUTE_SCALE"
  echo "compute_base_total_ms=$BASE_TOTAL_MS"
  echo "manifest=$MANIFEST"
  echo "plan=$PLAN"
  echo "symbolic_dir=$SYMBOLIC_DIR"
  echo "trace_dir=$TRACE_DIR"
  echo "uname=$(uname -a)"
  command -v lscpu >/dev/null 2>&1 && lscpu || true
  command -v rustc >/dev/null 2>&1 && rustc --version || true
  command -v cargo >/dev/null 2>&1 && cargo --version || true
  command -v go >/dev/null 2>&1 && go version || true
} > "$ENV_FILE"

WEIGHTS="$OUT_DIR/compute-weights.jsonl"
python3 tools/vegeta/build-native-s3-compute-weights.py \
  --execution-plan "$PLAN" \
  --source-traces-dir "$TRACE_DIR" \
  --output "$WEIGHTS" \
  --summary "$OUT_DIR/compute-weights-summary.json" \
  --max-missing-source "$ALLOWED_MISSING_SOURCE"

BIN="$OUT_DIR/bin/wasmd-scheduler-eval"
if [[ "$BUILD" == "1" ]]; then
  cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
  cargo build --manifest-path runtime/Cargo.toml -p acg-wasmd-scheduler-ffi --release
  [[ -s runtime/target/release/libacg_wasmd_scheduler_ffi.a ]] || { echo "missing Rust ACG static library" >&2; exit 3; }
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go mod download)
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && CGO_ENABLED=1 GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go build -tags acg_rust -o "$OUT_DIR_ABS/bin/wasmd-scheduler-eval" .)
else
  [[ -x "$BIN" ]] || { echo "EVAL_WASMD_BUILD=0 but binary is missing: $BIN" >&2; exit 3; }
fi

ITER_PER_NS="${EVAL_WASMD_GO_ITERATIONS_PER_NANO:-$($BIN --calibrate-only)}"
echo "Wasmd evaluation: mode=$MODE workers=$WORKERS_LIST samples=$SAMPLES physical_cores=$PHYSICAL_CORES iter/ns=$ITER_PER_NS"
"$BIN" --repo-root "$ROOT" --manifest "$MANIFEST" --plan "$PLAN" --setup-only

RECORDS="$OUT_DIR/records.jsonl"
: > "$RECORDS"
IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
for workers in "${WORKERS[@]}"; do
  raw="$OUT_DIR/raw/records-w${workers}.jsonl"
  echo "=== Wasmd scheduler campaign workers=$workers samples=$SAMPLES ==="
  "$BIN" \
    --repo-root "$ROOT" \
    --manifest "$MANIFEST" \
    --plan "$PLAN" \
    --compute-weights "$WEIGHTS" \
    --symbolic-dir "$SYMBOLIC_DIR" \
    --output "$raw" \
    --workers "$workers" \
    --samples "$SAMPLES" \
    --compute-scale "$COMPUTE_SCALE" \
    --compute-base-total-ms "$BASE_TOTAL_MS" \
    --go-iterations-per-nano "$ITER_PER_NS" \
    --symbgraph-rust-visibility mvcc \
    --symbgraph-rust-validation indexed \
    --symbgraph-rust-feedback profile
  cat "$raw" >> "$RECORDS"
done

python3 evaluation/wasmd/summarize.py --records "$RECORDS" --output-dir "$OUT_DIR/summary"
sha256sum "$RECORDS" > "$OUT_DIR/records.sha256"

echo
echo "Evaluation complete: $OUT_DIR"
echo "Primary report: $OUT_DIR/summary/summary.txt"
echo "Machine-readable summary: $OUT_DIR/summary/summary.csv"
echo "Per-sample metrics: $OUT_DIR/summary/per-sample.csv"
echo "Raw records: $RECORDS"
