#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_COMPUTE_SWEEP_DIR:-$EXEC_DIR/compute-calibration-sweep}"
METRICS="${VEGETA_S3_COMPUTE_METRICS:-steps,gas}"
SCALES="${VEGETA_S3_COMPUTE_SCALES:-0.25,0.5,1,2,4}"
WORKERS_LIST="${VEGETA_S3_COMPUTE_WORKERS:-1,2,4,8,16}"
SAMPLES="${VEGETA_S3_COMPUTE_SAMPLES:-1}"
BASE_TOTAL_MS="${VEGETA_S3_COMPUTE_BASE_TOTAL_MS:-1000}"
STRATEGIES="${VEGETA_S3_COMPUTE_STRATEGIES:-serial,exact-direct,exact-access}"
CUTOFF_MS="${VEGETA_S3_COMPUTE_CUTOFF_MS:-5000}"
ORDER_SEED="${VEGETA_S3_COMPUTE_ORDER_SEED:-2026082501}"
ALLOWED_MISSING_SOURCE="${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing exact source trace directory: $TRACE_DIR" >&2; exit 2; }

mkdir -p "$OUT_DIR/profiles"
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
  --bin acg-vegeta-native-s3-executor \
  --bin acg-vegeta-native-s3-benchmark \
  --release

ITERATIONS_PER_NANO="${VEGETA_S3_COMPUTE_ITERATIONS_PER_NANO:-$(runtime/target/release/acg-vegeta-compute-calibrate)}"
echo "compute calibration primitive: iterations/ns=$ITERATIONS_PER_NANO base-total-ms=$BASE_TOTAL_MS"

ALL_RECORDS="$OUT_DIR/records.jsonl"
: > "$ALL_RECORDS"

run_profile() {
  local metric="$1"
  local scale="$2"
  local label="$3"
  local profile_dir="$OUT_DIR/profiles/$label"
  local fidelity_dir="$profile_dir/cost-fidelity"
  mkdir -p "$fidelity_dir"
  echo "=== compute profile metric=$metric scale=$scale label=$label ==="

  runtime/target/release/acg-vegeta-native-s3-executor \
    --repo-root "$ROOT" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --plan "$EXEC_DIR/execution-plan.jsonl" \
    --output "$profile_dir/native-accesses.jsonl" \
    --compute-weights "$WEIGHTS" \
    --compute-metric "$metric" \
    --compute-scale "$scale" \
    --compute-base-total-ms "$BASE_TOTAL_MS" \
    --compute-iterations-per-nano "$ITERATIONS_PER_NANO"

  python3 tools/vegeta/analyze-native-s3-cost-fidelity.py \
    --native-accesses "$profile_dir/native-accesses.jsonl" \
    --source-traces-dir "$TRACE_DIR" \
    --output-dir "$fidelity_dir" \
    --max-missing-source "$ALLOWED_MISSING_SOURCE"

  IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
  for workers in "${WORKERS[@]}"; do
    local records="$profile_dir/records-workers-${workers}.jsonl"
    runtime/target/release/acg-vegeta-native-s3-benchmark \
      --repo-root "$ROOT" \
      --manifest "$EXEC_DIR/execution-manifest.json" \
      --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
      --symbolic-dir benchmarks/symbolic/native-s3 \
      --output "$records" \
      --workers "$workers" \
      --samples "$SAMPLES" \
      --consensus-cutoff-ms "$CUTOFF_MS" \
      --order-seed "$ORDER_SEED" \
      --strategies "$STRATEGIES" \
      --compute-weights "$WEIGHTS" \
      --compute-metric "$metric" \
      --compute-scale "$scale" \
      --compute-base-total-ms "$BASE_TOTAL_MS" \
      --compute-iterations-per-nano "$ITERATIONS_PER_NANO"
    cat "$records" >> "$ALL_RECORDS"
  done
}

# One semantic-only baseline, then source-weighted intensity sweeps.
run_profile none 0 none-0
IFS=',' read -r -a METRIC_LIST <<< "$METRICS"
IFS=',' read -r -a SCALE_LIST <<< "$SCALES"
for metric in "${METRIC_LIST[@]}"; do
  for scale in "${SCALE_LIST[@]}"; do
    label="${metric}-${scale//./p}"
    run_profile "$metric" "$scale" "$label"
  done
done

python3 tools/vegeta/summarize-native-s3-compute-sweep.py \
  --records "$ALL_RECORDS" \
  --profiles-root "$OUT_DIR/profiles" \
  --output-dir "$OUT_DIR"

python3 - "$OUT_DIR/scaling-summary.json" "$OUT_DIR/fidelity-summary.json" "$ALLOWED_MISSING_SOURCE" <<'PY'
import json,sys
scaling=json.load(open(sys.argv[1]))
fidelity=json.load(open(sys.argv[2]))
allowed=int(sys.argv[3])
assert scaling, 'empty compute scaling summary'
assert fidelity, 'empty compute fidelity summary'
assert all(row['serial_equivalent'] for row in scaling), 'state-equivalence failure in compute sweep'
assert all(row['missing_source_transactions'] <= allowed for row in fidelity), 'missing-source allowance exceeded'
print('PASS: compute calibration sweep is serial-equivalent and source joins stay within the frozen missing-trace allowance')
PY

cat "$OUT_DIR/summary.txt"
