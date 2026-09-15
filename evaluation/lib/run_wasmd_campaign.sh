#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

MODE="${1:-debug}"
case "$MODE" in
  smoke|debug|paper) ;;
  *) echo "usage: bash evaluation/lib/run_wasmd_campaign.sh [smoke|debug|paper]" >&2; exit 2 ;;
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
SETUP_CHECK="${EVAL_WASMD_SETUP_CHECK:-1}"
REUSE_SETUP_TEMPLATE="${EVAL_WASMD_REUSE_SETUP_TEMPLATE:-1}"
ISOLATE_STRATEGIES="${EVAL_WASMD_ISOLATE_STRATEGIES:-0}"
REUSE_ISOLATED_PARTS="${EVAL_WASMD_REUSE_ISOLATED_PARTS:-0}"
REUSE_WEIGHTS="${EVAL_WASMD_REUSE_WEIGHTS:-0}"
RESOURCE_ACCOUNTING="${EVAL_WASMD_RESOURCE_ACCOUNTING:-0}"
ONLY_STRATEGY="${EVAL_WASMD_ONLY_STRATEGY:-}"
GO_TOOLCHAIN="${EVAL_WASMD_GO_TOOLCHAIN:-auto}"
BASE_TOTAL_MS="${EVAL_WASMD_COMPUTE_BASE_TOTAL_MS:-1000}"
COMPUTE_SCALE="${EVAL_WASMD_COMPUTE_SCALE:-4}"
ALLOWED_MISSING_SOURCE="${EVAL_WASMD_ALLOWED_MISSING_SOURCE:-2}"
EXACT_ORACLE="${EVAL_WASMD_EXACT_ORACLE:-1}"
STREAM_PLAN="${EVAL_WASMD_STREAM_PLAN:-0}"
DATASET_LABEL="${EVAL_WASMD_DATASET:-vegeta-s3-wasmd-blockstm}"
SOURCE_CORPUS="${EVAL_WASMD_SOURCE_CORPUS:-}"
VEGETA_DATASET_TAG="${EVAL_WASMD_VEGETA_DATASET_TAG:-}"
MAX_BLOCKS="${EVAL_WASMD_MAX_BLOCKS:-0}"
CONSENSUS_WINDOWS_MS="${EVAL_WASMD_CONSENSUS_WINDOWS_MS:-${PAPER_EVAL_CONSENSUS_WINDOWS_MS:-${PAPER_EVAL_CONSENSUS_WINDOW_MS:-300}}}"
EXEC_DIR="${EVAL_WASMD_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${EVAL_WASMD_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
NATIVE_ACCESSES="${EVAL_WASMD_EXACT_NATIVE_ACCESSES:-$EXEC_DIR/native-accesses.jsonl}"
SYMBOLIC_DIR="${EVAL_WASMD_SYMBOLIC_DIR:-benchmarks/symbolic/native-s3}"
OUT_DIR="${EVAL_WASMD_OUTPUT_DIR:-benchmark-results/wasmd-${MODE}}"
RUST_ACG_ONLY="${VEGETA_S3_RUST_ACG_ONLY:-0}"
# Benchmark state-engine controls. EVAL_* is the stable script-facing interface;
# fall back to the older binary-level VEGETA_WASMD_* names for compatibility
# with existing S1 probe/debug wrappers.
IAVL_CACHE_SIZE="${EVAL_WASMD_IAVL_CACHE_SIZE:-${VEGETA_WASMD_IAVL_CACHE_SIZE:-500000}}"
IAVL_SYNC_PRUNING="${EVAL_WASMD_IAVL_SYNC_PRUNING:-${VEGETA_WASMD_IAVL_SYNC_PRUNING:-0}}"
[[ "$IAVL_CACHE_SIZE" =~ ^[0-9]+$ ]] || { echo "EVAL_WASMD_IAVL_CACHE_SIZE must be a non-negative integer" >&2; exit 2; }
case "${IAVL_SYNC_PRUNING,,}" in
  1|true|yes|on) IAVL_SYNC_PRUNING=1 ;;
  0|false|no|off) IAVL_SYNC_PRUNING=0 ;;
  *) echo "EVAL_WASMD_IAVL_SYNC_PRUNING must be 0/1 (or true/false)" >&2; exit 2 ;;
esac

ONLY_STRATEGY="$(printf '%s' "$ONLY_STRATEGY" | tr '[:upper:]' '[:lower:]')"
case "$ONLY_STRATEGY" in
  ""|serial|blockstm|ariafb|symbgraph-rust|vegeta|exact-oracle) ;;
  *) echo "EVAL_WASMD_ONLY_STRATEGY must be one of serial|blockstm|ariafb|symbgraph-rust|vegeta|exact-oracle" >&2; exit 2 ;;
esac
if [[ -n "$ONLY_STRATEGY" && "$ISOLATE_STRATEGIES" != "1" ]]; then
  echo "EVAL_WASMD_ONLY_STRATEGY requires EVAL_WASMD_ISOLATE_STRATEGIES=1" >&2
  exit 2
fi

MANIFEST="$EXEC_DIR/execution-manifest.json"
PLAN="$EXEC_DIR/execution-plan.jsonl"
for p in "$MANIFEST" "$PLAN"; do
  [[ -s "$p" ]] || { echo "missing evaluation input: $p" >&2; echo "prepare the frozen Wasmd translation before running this stage" >&2; exit 2; }
done
PLAN_BLOCKS="$(python3 - "$MANIFEST" "$PLAN" <<'PYBLOCKS'
import json, sys
manifest_path, plan_path = sys.argv[1:3]
obj = json.load(open(manifest_path, encoding="utf-8"))
try:
    blocks = int(obj.get("blocks", 0) or 0)
except (TypeError, ValueError):
    blocks = 0
if blocks <= 0:
    blocks = 0
    with open(plan_path, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"invalid execution plan JSON at line {lineno}: {exc}")
            if "block_number" not in row:
                raise SystemExit(f"execution plan row {lineno} is missing block_number")
            if not isinstance(row.get("transactions", []), list):
                raise SystemExit(f"execution plan row {lineno} has non-list transactions")
            blocks += 1
    if blocks <= 0:
        raise SystemExit("execution manifest has no positive blocks count and execution plan is empty")
    print(f"execution manifest has no positive blocks count; derived blocks={blocks} from execution plan", file=sys.stderr)
print(blocks)
PYBLOCKS
)"
if (( MAX_BLOCKS > 0 )); then
  EXPECTED_BLOCKS="$MAX_BLOCKS"
else
  EXPECTED_BLOCKS="$PLAN_BLOCKS"
fi
[[ -d "$SYMBOLIC_DIR" ]] || { echo "missing symbolic profile directory: $SYMBOLIC_DIR" >&2; exit 2; }
case "${STREAM_PLAN,,}" in
  1|true|yes|on) STREAM_PLAN=1 ;;
  0|false|no|off) STREAM_PLAN=0 ;;
  *) echo "EVAL_WASMD_STREAM_PLAN must be 0/1 (or true/false)" >&2; exit 2 ;;
esac
[[ "$MAX_BLOCKS" =~ ^[0-9]+$ ]] || { echo "EVAL_WASMD_MAX_BLOCKS must be a non-negative integer" >&2; exit 2; }

case "${EXACT_ORACLE,,}" in
  1|true|yes|on)
    EXACT_ORACLE=1
    [[ -s "$NATIVE_ACCESSES" ]] || { echo "missing exact-oracle native access audit: $NATIVE_ACCESSES" >&2; exit 2; }
    [[ -d "$TRACE_DIR" ]] || { echo "missing exact-oracle source-trace directory: $TRACE_DIR" >&2; exit 2; }
    ;;
  0|false|no|off) EXACT_ORACLE=0 ;;
  *) echo "EVAL_WASMD_EXACT_ORACLE must be 0/1 (or true/false)" >&2; exit 2 ;;
esac

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
  echo "exact_native_accesses=$NATIVE_ACCESSES"
  echo "rust_acg_only=$RUST_ACG_ONLY"
  echo "dataset=$DATASET_LABEL"
  echo "source_corpus=${SOURCE_CORPUS:-none}"
  echo "vegeta_dataset_tag=${VEGETA_DATASET_TAG:-none}"
  echo "exact_oracle=$EXACT_ORACLE"
  echo "stream_plan=$STREAM_PLAN"
  echo "setup_check=$SETUP_CHECK"
  echo "reuse_setup_template=$REUSE_SETUP_TEMPLATE"
  echo "isolate_strategies=$ISOLATE_STRATEGIES"
  echo "reuse_isolated_parts=$REUSE_ISOLATED_PARTS"
  echo "reuse_compute_weights=$REUSE_WEIGHTS"
  echo "only_strategy=${ONLY_STRATEGY:-all}"
  echo "max_blocks=$MAX_BLOCKS"
  if [[ "$CONSENSUS_WINDOWS_MS" == *,* ]]; then
    echo "consensus_window_mode=sensitivity"
    echo "consensus_windows_ms=$CONSENSUS_WINDOWS_MS"
  else
    echo "consensus_window_mode=fixed"
    echo "consensus_window_ms=$CONSENSUS_WINDOWS_MS"
  fi
  echo "iavl_cache_size=$IAVL_CACHE_SIZE"
  echo "iavl_sync_pruning=$IAVL_SYNC_PRUNING"
  echo "uname=$(uname -a)"
  command -v lscpu >/dev/null 2>&1 && lscpu || true
  command -v rustc >/dev/null 2>&1 && rustc --version || true
  command -v cargo >/dev/null 2>&1 && cargo --version || true
  command -v go >/dev/null 2>&1 && go version || true
} > "$ENV_FILE"

WEIGHTS="$OUT_DIR/compute-weights.jsonl"
WEIGHTS_SUMMARY="$OUT_DIR/compute-weights-summary.json"
WEIGHTS_META="$OUT_DIR/compute-weights.meta"
WEIGHT_BUILDER="tools/vegeta/build-native-s3-compute-weights.py"
PLAN_STAMP="$(stat -c '%s:%Y' "$PLAN" 2>/dev/null || stat -f '%z:%m' "$PLAN")"
weights_cache_ok() {
  [[ "$REUSE_WEIGHTS" == "1" && -s "$WEIGHTS" && -s "$WEIGHTS_SUMMARY" && -s "$WEIGHTS_META" ]] || return 1
  grep -Fxq "plan_stamp=$PLAN_STAMP" "$WEIGHTS_META" || return 1
  grep -Fxq "max_blocks=$MAX_BLOCKS" "$WEIGHTS_META" || return 1
  grep -Fxq "exact_oracle=$EXACT_ORACLE" "$WEIGHTS_META" || return 1
  grep -Fxq "allowed_missing_source=$ALLOWED_MISSING_SOURCE" "$WEIGHTS_META" || return 1
  [[ ! "$WEIGHT_BUILDER" -nt "$WEIGHTS" ]] || return 1
  [[ ! "$PLAN" -nt "$WEIGHTS" ]] || return 1
}
if weights_cache_ok; then
  echo "reusing cached compute weights: $WEIGHTS"
else
  if [[ "$EXACT_ORACLE" == "1" ]]; then
    python3 "$WEIGHT_BUILDER" \
      --execution-plan "$PLAN" \
      --source-traces-dir "$TRACE_DIR" \
      --output "$WEIGHTS" \
      --summary "$WEIGHTS_SUMMARY" \
      --max-missing-source "$ALLOWED_MISSING_SOURCE" \
      --max-blocks "$MAX_BLOCKS"
  else
    python3 "$WEIGHT_BUILDER" \
      --execution-plan "$PLAN" \
      --fallback-plan-gas \
      --output "$WEIGHTS" \
      --summary "$WEIGHTS_SUMMARY" \
      --max-blocks "$MAX_BLOCKS"
  fi
  cat > "$WEIGHTS_META" <<EOF
plan_stamp=$PLAN_STAMP
max_blocks=$MAX_BLOCKS
exact_oracle=$EXACT_ORACLE
allowed_missing_source=$ALLOWED_MISSING_SOURCE
EOF
fi

COMPUTE_METRIC="$(python3 - "$WEIGHTS_SUMMARY" <<'PYMETRIC'
import json, sys
obj=json.load(open(sys.argv[1], encoding="utf-8"))
print(str(obj.get("compute_metric") or "cost"))
PYMETRIC
)"

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

BINARY_SHA256="$(sha256sum "$BIN" | awk '{print $1}')"
ITER_PER_NS="${EVAL_WASMD_GO_ITERATIONS_PER_NANO:-$($BIN --calibrate-only)}"
echo "Wasmd evaluation: mode=$MODE workers=$WORKERS_LIST samples=$SAMPLES physical_cores=$PHYSICAL_CORES iter/ns=$ITER_PER_NS"
if [[ "$SETUP_CHECK" == "1" ]]; then
  "$BIN" --repo-root "$ROOT" --manifest "$MANIFEST" --plan "$PLAN" --dataset "$DATASET_LABEL" --exact-oracle="$EXACT_ORACLE" --stream-plan="$STREAM_PLAN" --max-blocks "$MAX_BLOCKS" --reuse-setup-template="$REUSE_SETUP_TEMPLATE" --iavl-cache-size "$IAVL_CACHE_SIZE" --iavl-sync-pruning="$IAVL_SYNC_PRUNING" --setup-only
else
  echo "skipping redundant standalone setup-only pass; campaign setup template will validate Wasmd initialization"
fi

RECORDS="$OUT_DIR/records.jsonl"
: > "$RECORDS"
IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
run_campaign() {
  local workers="$1" output="$2" strategy="$3" serial_oracle="${4:-}" resource_out="${5:-}"
  local args=(
    --repo-root "$ROOT"
    --manifest "$MANIFEST"
    --dataset "$DATASET_LABEL"
    --exact-oracle="$EXACT_ORACLE"
    --stream-plan="$STREAM_PLAN"
    --max-blocks "$MAX_BLOCKS"
    --reuse-setup-template="$REUSE_SETUP_TEMPLATE"
    --campaign-strategy "$strategy"
    --plan "$PLAN"
    --compute-weights "$WEIGHTS"
    --symbolic-dir "$SYMBOLIC_DIR"
    --exact-trace-dir "$TRACE_DIR"
    --exact-native-accesses "$NATIVE_ACCESSES"
    --output "$output"
    --workers "$workers"
    --samples "$SAMPLES"
    --compute-scale "$COMPUTE_SCALE"
    --compute-base-total-ms "$BASE_TOTAL_MS"
    --go-iterations-per-nano "$ITER_PER_NS"
    --iavl-cache-size "$IAVL_CACHE_SIZE"
    --iavl-sync-pruning="$IAVL_SYNC_PRUNING"
    --symbgraph-rust-visibility mvcc
    --symbgraph-rust-validation indexed
    --symbgraph-rust-feedback profile
  )
  if [[ -n "$serial_oracle" ]]; then
    args+=(--serial-oracle "$serial_oracle")
  fi
  if [[ -n "${EVAL_WASMD_CAMPAIGN_PROFILE_DIR:-}" ]]; then
    args+=(--campaign-profile-dir "$EVAL_WASMD_CAMPAIGN_PROFILE_DIR")
  fi
  if [[ -n "$resource_out" ]]; then
    [[ -x /usr/bin/time ]] || { echo "GNU /usr/bin/time is required when EVAL_WASMD_RESOURCE_ACCOUNTING=1" >&2; exit 3; }
    LC_ALL=C /usr/bin/time -v -o "$resource_out" "$BIN" "${args[@]}"
  else
    "$BIN" "${args[@]}"
  fi
}

isolated_part_complete() {
  local part="$1" strategy="$2" workers="$3" meta="$part.meta"
  [[ "$REUSE_ISOLATED_PARTS" == "1" && -s "$part" && -s "$meta" ]] || return 1
  if [[ "$RESOURCE_ACCOUNTING" == "1" && ! -s "$part.resource.txt" ]]; then return 1; fi
  local plan_stamp
  plan_stamp="$(stat -c '%s:%Y' "$PLAN" 2>/dev/null || stat -f '%z:%m' "$PLAN")"
  grep -Fxq "strategy=$strategy" "$meta" || return 1
  grep -Fxq "workers=$workers" "$meta" || return 1
  grep -Fxq "samples=$SAMPLES" "$meta" || return 1
  grep -Fxq "compute_scale=$COMPUTE_SCALE" "$meta" || return 1
  grep -Fxq "compute_base_total_ms=$BASE_TOTAL_MS" "$meta" || return 1
  grep -Fxq "iterations_per_nano=$ITER_PER_NS" "$meta" || return 1
  grep -Fxq "max_blocks=$MAX_BLOCKS" "$meta" || return 1
  grep -Fxq "expected_blocks=$EXPECTED_BLOCKS" "$meta" || return 1
  grep -Fxq "dataset=$DATASET_LABEL" "$meta" || return 1
  grep -Fxq "exact_oracle=$EXACT_ORACLE" "$meta" || return 1
  grep -Fxq "plan_stamp=$plan_stamp" "$meta" || return 1
  grep -Fxq "binary_sha256=$BINARY_SHA256" "$meta" || return 1
  grep -Fxq "iavl_cache_size=$IAVL_CACHE_SIZE" "$meta" || return 1
  grep -Fxq "iavl_sync_pruning=$IAVL_SYNC_PRUNING" "$meta" || return 1
  if [[ "$strategy" != "serial" ]]; then
    local serial_part="$OUT_DIR/raw/records-w${workers}-serial.jsonl" serial_hash
    [[ -s "$serial_part" ]] || return 1
    serial_hash="$(sha256sum "$serial_part" | awk '{print $1}')"
    grep -Fxq "serial_oracle_sha256=$serial_hash" "$meta" || return 1
  fi
  python3 - "$part" "$strategy" "$workers" "$SAMPLES" "$EXPECTED_BLOCKS" <<'PYREC'
import json, sys
from collections import defaultdict

path, campaign, workers, samples, blocks = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])
labels = {
    "serial": "cosmos-wasmd-direct-serial",
    "blockstm": "cosmos-wasmd-block-stm",
    "ariafb": "cosmos-wasmd-aria-fb",
    "symbgraph-rust": "cosmos-wasmd-symbgraph-rust",
    "vegeta": "cosmos-wasmd-vegeta",
    "exact-oracle": "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
}
expected = labels[campaign]
seen = defaultdict(set)
rows = 0
try:
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            if row.get("strategy") != expected or int(row.get("workers", -1)) != workers:
                raise ValueError("strategy/workers mismatch")
            if campaign == "serial":
                if int(row.get("serial_commit_version", 0)) <= 0 or not row.get("serial_commit_hash"):
                    raise ValueError("serial part lacks persisted CommitID oracle fields")
            sample = int(row.get("sample", -1))
            if not 0 <= sample < samples:
                raise ValueError("sample out of range")
            block = int(row["block_number"])
            if block in seen[sample]:
                raise ValueError("duplicate block")
            seen[sample].add(block)
            rows += 1
except Exception:
    raise SystemExit(1)
if rows != samples * blocks or any(len(seen[s]) != blocks for s in range(samples)):
    raise SystemExit(1)
PYREC
}

write_isolated_part_meta() {
  local part="$1" strategy="$2" workers="$3" plan_stamp
  plan_stamp="$(stat -c '%s:%Y' "$PLAN" 2>/dev/null || stat -f '%z:%m' "$PLAN")"
  cat > "$part.meta" <<EOF
strategy=$strategy
workers=$workers
samples=$SAMPLES
compute_scale=$COMPUTE_SCALE
compute_base_total_ms=$BASE_TOTAL_MS
iterations_per_nano=$ITER_PER_NS
max_blocks=$MAX_BLOCKS
expected_blocks=$EXPECTED_BLOCKS
dataset=$DATASET_LABEL
exact_oracle=$EXACT_ORACLE
plan_stamp=$plan_stamp
binary_sha256=$BINARY_SHA256
resource_accounting=$RESOURCE_ACCOUNTING
iavl_cache_size=$IAVL_CACHE_SIZE
iavl_sync_pruning=$IAVL_SYNC_PRUNING
EOF
  if [[ "$strategy" != "serial" ]]; then
    local serial_part="$OUT_DIR/raw/records-w${workers}-serial.jsonl" serial_hash
    serial_hash="$(sha256sum "$serial_part" | awk '{print $1}')"
    printf 'serial_oracle_sha256=%s\n' "$serial_hash" >> "$part.meta"
  fi
}

for workers in "${WORKERS[@]}"; do
  raw="$OUT_DIR/raw/records-w${workers}.jsonl"
  : > "$raw"
  echo "=== Wasmd scheduler campaign workers=$workers samples=$SAMPLES ==="
  if [[ "$ISOLATE_STRATEGIES" == "1" ]]; then
    if [[ -n "$ONLY_STRATEGY" ]]; then
      strategies=("$ONLY_STRATEGY")
    else
      strategies=(serial blockstm ariafb symbgraph-rust vegeta)
      if [[ "$EXACT_ORACLE" != "0" ]]; then strategies+=(exact-oracle); fi
    fi
    echo "isolating scheduler strategies into separate processes to bound live Wasmd state"
    for strategy in "${strategies[@]}"; do
      part="$OUT_DIR/raw/records-w${workers}-${strategy}.jsonl"
      echo "--- Wasmd isolated strategy=$strategy workers=$workers ---"
      if isolated_part_complete "$part" "$strategy" "$workers"; then
        echo "reusing completed isolated strategy=$strategy workers=$workers"
      else
        : > "$part"
        rm -f "$part.meta"
        serial_oracle=""
        if [[ "$strategy" != "serial" ]]; then
          serial_oracle="$OUT_DIR/raw/records-w${workers}-serial.jsonl"
          [[ -s "$serial_oracle" ]] || { echo "missing completed serial oracle: $serial_oracle" >&2; exit 3; }
        fi
        resource_out=""
        if [[ "$RESOURCE_ACCOUNTING" == "1" ]]; then resource_out="$part.resource.txt"; rm -f "$resource_out"; fi
        run_campaign "$workers" "$part" "$strategy" "$serial_oracle" "$resource_out"
        write_isolated_part_meta "$part" "$strategy" "$workers"
      fi
      cat "$part" >> "$raw"
    done
  else
    run_campaign "$workers" "$raw" all
  fi
  cat "$raw" >> "$RECORDS"
done

if [[ -n "$ONLY_STRATEGY" ]]; then
  sha256sum "$RECORDS" > "$OUT_DIR/records.sha256"
  echo
  echo "Evaluation strategy complete: strategy=$ONLY_STRATEGY output=$OUT_DIR"
  echo "Raw strategy rows: $OUT_DIR/raw/records-w${WORKERS[0]}-${ONLY_STRATEGY}.jsonl"
  exit 0
fi

SUMMARY_ARGS=(--records "$RECORDS" --output-dir "$OUT_DIR/summary" --cost-metric "$COMPUTE_METRIC" --consensus-windows-ms "$CONSENSUS_WINDOWS_MS")
case "${RUST_ACG_ONLY,,}" in
  1|true|yes|on) SUMMARY_ARGS+=(--rust-acg-only) ;;
esac
if [[ "$EXACT_ORACLE" == "0" ]]; then SUMMARY_ARGS+=(--no-exact-oracle); fi
if [[ -n "$SOURCE_CORPUS" ]]; then SUMMARY_ARGS+=(--source-corpus "$SOURCE_CORPUS"); fi
if [[ -n "$VEGETA_DATASET_TAG" ]]; then SUMMARY_ARGS+=(--vegeta-dataset-tag "$VEGETA_DATASET_TAG"); fi
python3 evaluation/wasmd/summarize.py "${SUMMARY_ARGS[@]}"
if [[ "$RESOURCE_ACCOUNTING" == "1" ]]; then
  python3 evaluation/eurosys/summarize_resource_usage.py --raw-dir "$OUT_DIR/raw" --output-dir "$OUT_DIR/summary"
fi
sha256sum "$RECORDS" > "$OUT_DIR/records.sha256"

echo
echo "Evaluation complete: $OUT_DIR"
echo "Primary report: $OUT_DIR/summary/summary.txt"
echo "Machine-readable summary: $OUT_DIR/summary/summary.csv"
echo "Per-sample metrics: $OUT_DIR/summary/per-sample.csv"
echo "Raw records: $RECORDS"
