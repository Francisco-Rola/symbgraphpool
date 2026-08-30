#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_RUNTIME_PROFILE_DIR:-$EXEC_DIR/runtime-concurrency-profile}"
WORKERS_LIST="${VEGETA_S3_PROFILE_WORKERS:-1,2,4,8,16}"
SAMPLES="${VEGETA_S3_PROFILE_SAMPLES:-1}"
BASE_TOTAL_MS="${VEGETA_S3_COMPUTE_BASE_TOTAL_MS:-1000}"
CUTOFF_MS="${VEGETA_S3_PROFILE_CUTOFF_MS:-5000}"
ORDER_SEED="${VEGETA_S3_PROFILE_ORDER_SEED:-2026082501}"
ALLOWED_MISSING_SOURCE="${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"
STRATEGIES="${VEGETA_S3_PROFILE_STRATEGIES:-serial,exact-direct,exact-access}"
PERF_MODE="${VEGETA_S3_PROFILE_PERF:-auto}"
PERF_STRATEGY="${VEGETA_S3_PROFILE_PERF_STRATEGY:-exact-direct}"
PROFILE_SET="${VEGETA_S3_PROFILE_PROFILES:-none-0,steps-4,gas-4}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing exact source trace directory: $TRACE_DIR" >&2; exit 2; }

mkdir -p "$OUT_DIR/profiles" "$OUT_DIR/perf" "$OUT_DIR/perf-records"
WEIGHTS="$OUT_DIR/compute-weights.jsonl"
WEIGHTS_SUMMARY="$OUT_DIR/compute-weights-summary.json"
python3 tools/vegeta/build-native-s3-compute-weights.py \
  --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
  --source-traces-dir "$TRACE_DIR" \
  --output "$WEIGHTS" \
  --summary "$WEIGHTS_SUMMARY" \
  --max-missing-source "$ALLOWED_MISSING_SOURCE"

cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml \
  -p acg-vegeta-native-s3-executor \
  --bin acg-vegeta-compute-calibrate \
  --bin acg-vegeta-native-s3-benchmark \
  --release

ITERATIONS_PER_NANO="${VEGETA_S3_COMPUTE_ITERATIONS_PER_NANO:-$(runtime/target/release/acg-vegeta-compute-calibrate)}"
echo "compute primitive iterations/ns=$ITERATIONS_PER_NANO base-total-ms=$BASE_TOTAL_MS"

ALL_RECORDS="$OUT_DIR/records.jsonl"
: > "$ALL_RECORDS"

resolve_perf() {
  local candidate
  if command -v perf >/dev/null 2>&1; then
    candidate="$(command -v perf)"
    if "$candidate" --version >/dev/null 2>&1; then
      printf '%s\n' "$candidate"; return 0
    fi
  fi
  while IFS= read -r candidate; do
    if "$candidate" --version >/dev/null 2>&1; then
      printf '%s\n' "$candidate"; return 0
    fi
  done < <(find /usr/lib/linux-tools -type f -name perf -perm -111 2>/dev/null | sort -V -r)
  return 1
}

PERF_BIN="${VEGETA_S3_PERF_BIN:-}"
if [[ -z "$PERF_BIN" ]]; then PERF_BIN="$(resolve_perf || true)"; fi
PERF_ENABLED=0
PERF_EVENTS_MODE="none"
PERF_EVENTS=""
if [[ "$PERF_MODE" != "0" && "$PERF_MODE" != "off" && -n "$PERF_BIN" ]]; then
  SOFTWARE_EVENTS="task-clock,context-switches,cpu-migrations,page-faults"
  HARDWARE_EVENTS="$SOFTWARE_EVENTS,cycles,instructions,branches,branch-misses,cache-references,cache-misses"
  if LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$HARDWARE_EVENTS" -- true >/dev/null 2>&1; then
    PERF_ENABLED=1; PERF_EVENTS_MODE="hardware+software"; PERF_EVENTS="$HARDWARE_EVENTS"
  elif LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$SOFTWARE_EVENTS" -- true >/dev/null 2>&1; then
    PERF_ENABLED=1; PERF_EVENTS_MODE="software-only"; PERF_EVENTS="$SOFTWARE_EVENTS"
  elif [[ "$PERF_MODE" == "1" || "$PERF_MODE" == "on" || "$PERF_MODE" == "required" ]]; then
    echo "perf is installed but perf_event access is unavailable; see evaluation/vegeta/S3_RUNTIME_CONCURRENCY_PROFILING.md" >&2
    exit 3
  else
    echo "warning: perf unavailable or blocked; continuing with internal runtime diagnostics only" >&2
  fi
elif [[ "$PERF_MODE" == "1" || "$PERF_MODE" == "on" || "$PERF_MODE" == "required" ]]; then
  echo "perf requested but no usable perf binary was found" >&2; exit 3
fi
if (( PERF_ENABLED )); then echo "perf enabled: $PERF_BIN events=$PERF_EVENTS_MODE"; fi

run_benchmark() {
  local metric="$1" scale="$2" workers="$3" output="$4" strategies="$5"
  runtime/target/release/acg-vegeta-native-s3-benchmark \
    --repo-root "$ROOT" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --symbolic-dir benchmarks/symbolic/native-s3 \
    --output "$output" \
    --workers "$workers" \
    --samples "$SAMPLES" \
    --consensus-cutoff-ms "$CUTOFF_MS" \
    --order-seed "$ORDER_SEED" \
    --strategies "$strategies" \
    --compute-weights "$WEIGHTS" \
    --compute-metric "$metric" \
    --compute-scale "$scale" \
    --compute-base-total-ms "$BASE_TOTAL_MS" \
    --compute-iterations-per-nano "$ITERATIONS_PER_NANO" \
    --runtime-profile
}

run_perf() {
  local metric="$1" scale="$2" label="$3" workers="$4"
  (( PERF_ENABLED )) || return 0
  local perf_file="$OUT_DIR/perf/perf-${label}-workers-${workers}-${PERF_STRATEGY}.csv"
  local meta_file="$OUT_DIR/perf/perf-${label}-workers-${workers}-${PERF_STRATEGY}.meta.json"
  local records="$OUT_DIR/perf-records/${label}-workers-${workers}-${PERF_STRATEGY}.jsonl"
  local started ended elapsed
  started="$(date +%s%N)"
  LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$PERF_EVENTS" -o "$perf_file" -- \
    runtime/target/release/acg-vegeta-native-s3-benchmark \
      --repo-root "$ROOT" \
      --manifest "$EXEC_DIR/execution-manifest.json" \
      --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
      --symbolic-dir benchmarks/symbolic/native-s3 \
      --output "$records" \
      --workers "$workers" \
      --samples 1 \
      --consensus-cutoff-ms "$CUTOFF_MS" \
      --order-seed "$ORDER_SEED" \
      --strategies "$PERF_STRATEGY" \
      --compute-weights "$WEIGHTS" \
      --compute-metric "$metric" \
      --compute-scale "$scale" \
      --compute-base-total-ms "$BASE_TOTAL_MS" \
      --compute-iterations-per-nano "$ITERATIONS_PER_NANO" \
      --runtime-profile
  ended="$(date +%s%N)"; elapsed=$((ended-started))
  python3 - "$meta_file" "$metric" "$scale" "$workers" "$PERF_STRATEGY" "$elapsed" "$PERF_EVENTS_MODE" <<'PY'
import json,sys
path,metric,scale,workers,strategy,elapsed,mode=sys.argv[1:]
json.dump({'metric': 'gas-used' if metric == 'gas' else metric, 'scale': float(scale),
           'workers': int(workers), 'strategy': strategy, 'elapsed_nanos': int(elapsed),
           'events_mode': mode}, open(path,'w'), indent=2)
PY
}

run_profile() {
  local metric="$1" scale="$2" label="$3"
  IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
  for workers in "${WORKERS[@]}"; do
    echo "=== runtime profile metric=$metric scale=$scale workers=$workers ==="
    local records="$OUT_DIR/profiles/${label}-workers-${workers}.jsonl"
    run_benchmark "$metric" "$scale" "$workers" "$records" "$STRATEGIES"
    cat "$records" >> "$ALL_RECORDS"
    run_perf "$metric" "$scale" "$label" "$workers"
  done
}

IFS=',' read -r -a PROFILES <<< "$PROFILE_SET"
for profile in "${PROFILES[@]}"; do
  case "$profile" in
    none-0) run_profile none 0 none-0 ;;
    steps-4) run_profile steps 4 steps-4 ;;
    gas-4) run_profile gas 4 gas-4 ;;
    *) echo "unknown VEGETA_S3_PROFILE_PROFILES entry: $profile" >&2; exit 2 ;;
  esac
done

python3 tools/vegeta/summarize-native-s3-runtime-profile.py \
  --records "$ALL_RECORDS" \
  --perf-dir "$OUT_DIR/perf" \
  --output-dir "$OUT_DIR"

python3 - "$OUT_DIR/runtime-profile-summary.json" "$STRATEGIES" <<'PY'
import json,sys
rows=json.load(open(sys.argv[1]))
requested={s.strip() for s in sys.argv[2].split(',') if s.strip()}
assert rows, 'empty runtime profile summary'
if 'exact-direct' in requested:
    assert any(r['strategy']=='exact-direct' and r['profile_kind']=='exact-direct' for r in rows), 'missing direct diagnostics'
if 'exact-access' in requested:
    assert any(r['strategy']=='exact-access' and r['profile_kind']=='dependency-mvcc' for r in rows), 'missing MVCC diagnostics'
print('PASS: runtime profile contains diagnostics for all requested profiled strategies')
PY
