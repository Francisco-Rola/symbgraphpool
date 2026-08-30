#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_PUBLICATION_MATRIX_DIR:-$EXEC_DIR/publication-matrix-steps4}"
MODE="${VEGETA_S3_PUBLICATION_MODE:-debug}"
case "$MODE" in
  smoke) DEFAULT_WORKERS="2"; DEFAULT_SAMPLES="1" ;;
  debug) DEFAULT_WORKERS="2,4"; DEFAULT_SAMPLES="1" ;;
  paper) DEFAULT_WORKERS="1,2,4,8,16"; DEFAULT_SAMPLES="5" ;;
  *) echo "unknown VEGETA_S3_PUBLICATION_MODE=$MODE (use smoke|debug|paper)" >&2; exit 2 ;;
esac
WORKERS_LIST="${VEGETA_S3_PUBLICATION_WORKERS:-$DEFAULT_WORKERS}"
SAMPLES="${VEGETA_S3_PUBLICATION_SAMPLES:-$DEFAULT_SAMPLES}"
BASE_TOTAL_MS="${VEGETA_S3_COMPUTE_BASE_TOTAL_MS:-1000}"
CUTOFF_MS="${VEGETA_S3_PUBLICATION_CUTOFF_MS:-5000}"
ORDER_SEED="${VEGETA_S3_PUBLICATION_ORDER_SEED:-2026082501}"
ALLOWED_MISSING_SOURCE="${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"
STRATEGIES="${VEGETA_S3_PUBLICATION_STRATEGIES:-serial,aria-fb,vegeta,static,probability-only,cost-aware,exact-direct,exact-access}"
COSMOS_ACCESS="${VEGETA_S3_PUBLICATION_COSMOS_BLOCKSTM:-1}"
COSMOS_WASMD="${VEGETA_S3_PUBLICATION_COSMOS_WASMD_BLOCKSTM:-1}"
COSMOS_GO_TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
WASMD_INVESTIGATE="${VEGETA_S3_WASMD_INVESTIGATE:-0}" # old double-cache + optimized tracked-single-cache controls
WASMD_COMPARE_PPROF_DIR="${VEGETA_S3_WASMD_COMPARE_PPROF_DIR:-}"
RUST_ACG_VISIBILITY="${VEGETA_S3_RUST_ACG_VISIBILITY:-mvcc}"
RUST_ACG_VALIDATION="${VEGETA_S3_RUST_ACG_VALIDATION:-indexed}"
RUST_ACG_FEEDBACK="${VEGETA_S3_RUST_ACG_FEEDBACK:-profile}"
RUST_ACG_EDGE_MATERIALIZATION_THRESHOLD="${VEGETA_S3_RUST_ACG_EDGE_MATERIALIZATION_THRESHOLD:-}"
RUST_ACG_SOFT_THRESHOLD="${VEGETA_S3_RUST_ACG_SOFT_THRESHOLD:-}"
RUST_ACG_HARD_THRESHOLD="${VEGETA_S3_RUST_ACG_HARD_THRESHOLD:-}"
RUST_ACG_RISK_BUDGET="${VEGETA_S3_RUST_ACG_RISK_BUDGET:-}"
RUST_ACG_EXPLORATION_RATE="${VEGETA_S3_RUST_ACG_EXPLORATION_RATE:-}"
RUST_ACG_EXPLORATION_RISK_BUDGET="${VEGETA_S3_RUST_ACG_EXPLORATION_RISK_BUDGET:-}"
RUST_ACG_EXPLORATION_MIN_UNCERTAINTY="${VEGETA_S3_RUST_ACG_EXPLORATION_MIN_UNCERTAINTY:-}"
RUST_ACG_EXPLORATION_MAX_TRANSACTIONS="${VEGETA_S3_RUST_ACG_EXPLORATION_MAX_TRANSACTIONS:-}"
RUST_ACG_INDEPENDENCE_BEFORE_SOFTENING="${VEGETA_S3_RUST_ACG_INDEPENDENCE_BEFORE_SOFTENING:-}"
RUST_ACG_SOFTENING_MIN_CONFIDENCE="${VEGETA_S3_RUST_ACG_SOFTENING_MIN_CONFIDENCE:-}"
RUST_ACG_DEPENDENCY_DIAGNOSTICS="${VEGETA_S3_RUST_ACG_DEPENDENCY_DIAGNOSTICS:-0}"
if [[ "$OUT_DIR" = /* ]]; then OUT_DIR_ABS="$OUT_DIR"; else OUT_DIR_ABS="$ROOT/$OUT_DIR"; fi

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing exact source trace directory: $TRACE_DIR" >&2; exit 2; }
mkdir -p "$OUT_DIR/native" "$OUT_DIR/cosmos-block-stm" "$OUT_DIR/cosmos-wasmd-block-stm"

echo "publication matrix mode=$MODE workers=$WORKERS_LIST samples=$SAMPLES"
echo "Rust-ACG Wasmd variant visibility=$RUST_ACG_VISIBILITY validation=$RUST_ACG_VALIDATION feedback=$RUST_ACG_FEEDBACK"
RUST_POLICY_ARGS=()
add_rust_policy_arg() { [[ -n "$2" ]] && RUST_POLICY_ARGS+=("$1" "$2"); }
add_rust_policy_arg --symbgraph-rust-edge-materialization-threshold "$RUST_ACG_EDGE_MATERIALIZATION_THRESHOLD"
add_rust_policy_arg --symbgraph-rust-soft-threshold "$RUST_ACG_SOFT_THRESHOLD"
add_rust_policy_arg --symbgraph-rust-hard-threshold "$RUST_ACG_HARD_THRESHOLD"
add_rust_policy_arg --symbgraph-rust-risk-budget "$RUST_ACG_RISK_BUDGET"
add_rust_policy_arg --symbgraph-rust-exploration-rate "$RUST_ACG_EXPLORATION_RATE"
add_rust_policy_arg --symbgraph-rust-exploration-risk-budget "$RUST_ACG_EXPLORATION_RISK_BUDGET"
add_rust_policy_arg --symbgraph-rust-exploration-min-uncertainty "$RUST_ACG_EXPLORATION_MIN_UNCERTAINTY"
add_rust_policy_arg --symbgraph-rust-exploration-max-transactions "$RUST_ACG_EXPLORATION_MAX_TRANSACTIONS"
add_rust_policy_arg --symbgraph-rust-independence-before-softening "$RUST_ACG_INDEPENDENCE_BEFORE_SOFTENING"
add_rust_policy_arg --symbgraph-rust-softening-min-confidence "$RUST_ACG_SOFTENING_MIN_CONFIDENCE"
if (( ${#RUST_POLICY_ARGS[@]} )); then printf 'Rust-ACG policy overrides:'; printf ' %q' "${RUST_POLICY_ARGS[@]}"; printf '
'; fi

WEIGHTS="$OUT_DIR/compute-weights.jsonl"
python3 tools/vegeta/build-native-s3-compute-weights.py \
  --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
  --source-traces-dir "$TRACE_DIR" \
  --output "$WEIGHTS" \
  --summary "$OUT_DIR/compute-weights-summary.json" \
  --max-missing-source "$ALLOWED_MISSING_SOURCE"

cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml \
  -p acg-vegeta-native-s3-executor \
  --bin acg-vegeta-compute-calibrate \
  --bin acg-vegeta-native-s3-executor \
  --bin acg-vegeta-native-s3-benchmark \
  --release
RUST_ITER_PER_NS="${VEGETA_S3_COMPUTE_ITERATIONS_PER_NANO:-$(runtime/target/release/acg-vegeta-compute-calibrate)}"
echo "steps-4 Rust compute calibration iterations/ns=$RUST_ITER_PER_NS"

# Export the exact concrete native access stream once. DeterministicCompute has no
# accesses, so this is the same dependency topology used by the native steps-4 runs.
NATIVE_ACCESS="$OUT_DIR/native-accesses-steps4.jsonl"
runtime/target/release/acg-vegeta-native-s3-executor \
  --repo-root "$ROOT" \
  --manifest "$EXEC_DIR/execution-manifest.json" \
  --plan "$EXEC_DIR/execution-plan.jsonl" \
  --output "$NATIVE_ACCESS" \
  --compute-weights "$WEIGHTS" \
  --compute-metric steps \
  --compute-scale 4 \
  --compute-base-total-ms "$BASE_TOTAL_MS" \
  --compute-iterations-per-nano "$RUST_ITER_PER_NS"

python3 tools/vegeta/analyze-native-s3-cost-fidelity.py \
  --native-accesses "$NATIVE_ACCESS" \
  --source-traces-dir "$TRACE_DIR" \
  --output-dir "$OUT_DIR/cost-fidelity" \
  --max-missing-source "$ALLOWED_MISSING_SOURCE"

NATIVE_RECORDS="$OUT_DIR/native-records.jsonl"; : > "$NATIVE_RECORDS"
COSMOS_RECORDS="$OUT_DIR/cosmos-block-stm-records.jsonl"; : > "$COSMOS_RECORDS"
WASMD_RECORDS="$OUT_DIR/cosmos-wasmd-block-stm-records.jsonl"; : > "$WASMD_RECORDS"

if [[ "$COSMOS_ACCESS" != "0" && "$COSMOS_ACCESS" != "off" ]]; then
  command -v go >/dev/null 2>&1 || { echo "Go is required for the Cosmos Block-STM baselines" >&2; exit 3; }
  echo "preparing Cosmos SDK Block-STM access-replay module"
  (cd benchmarks/cosmos-blockstm-s3 && GOTOOLCHAIN="$COSMOS_GO_TOOLCHAIN" GOFLAGS=-mod=mod go mod download)
  (cd benchmarks/cosmos-blockstm-s3 && GOTOOLCHAIN="$COSMOS_GO_TOOLCHAIN" GOFLAGS=-mod=mod go build -o "$OUT_DIR_ABS/cosmos-block-stm/blockstm-s3" .)
  BLOCKSTM_BIN="$OUT_DIR_ABS/cosmos-block-stm/blockstm-s3"
  GO_ITER_PER_NS="${VEGETA_S3_BLOCKSTM_GO_ITERATIONS_PER_NANO:-$($BLOCKSTM_BIN --calibrate-only)}"
  echo "Cosmos Block-STM access-replay Go compute calibration iterations/ns=$GO_ITER_PER_NS"
fi

if [[ "$COSMOS_WASMD" != "0" && "$COSMOS_WASMD" != "off" ]]; then
  command -v go >/dev/null 2>&1 || { echo "Go is required for the Wasmd Block-STM baseline" >&2; exit 3; }
  echo "preparing Rust ACG bridge + Wasmd v0.70.3 / Cosmos SDK v0.54.4 module"
  cargo build --manifest-path runtime/Cargo.toml -p acg-wasmd-scheduler-ffi --release
  [[ -s runtime/target/release/libacg_wasmd_scheduler_ffi.a ]] || { echo "missing Rust scheduler staticlib" >&2; exit 3; }
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && GOTOOLCHAIN="$COSMOS_GO_TOOLCHAIN" GOFLAGS=-mod=mod go mod download)
  (cd benchmarks/cosmos-wasmd-blockstm-s3 && CGO_ENABLED=1 GOTOOLCHAIN="$COSMOS_GO_TOOLCHAIN" GOFLAGS=-mod=mod go build -tags acg_rust -o "$OUT_DIR_ABS/cosmos-wasmd-block-stm/wasmd-blockstm-s3" .)
  WASMD_BLOCKSTM_BIN="$OUT_DIR_ABS/cosmos-wasmd-block-stm/wasmd-blockstm-s3"
  WASMD_GO_ITER_PER_NS="${VEGETA_S3_WASMD_BLOCKSTM_GO_ITERATIONS_PER_NANO:-$($WASMD_BLOCKSTM_BIN --calibrate-only)}"
  echo "Wasmd/WasmVM Block-STM Go compute calibration iterations/ns=$WASMD_GO_ITER_PER_NS"
  "$WASMD_BLOCKSTM_BIN" --repo-root "$ROOT" \
    --manifest "$EXEC_DIR/execution-manifest.json" --plan "$EXEC_DIR/execution-plan.jsonl" --setup-only
fi

IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
for workers in "${WORKERS[@]}"; do
  echo "=== publication matrix steps-4 workers=$workers samples=$SAMPLES ==="
  records="$OUT_DIR/native/records-workers-${workers}.jsonl"
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
    --compute-metric steps \
    --compute-scale 4 \
    --compute-base-total-ms "$BASE_TOTAL_MS" \
    --compute-iterations-per-nano "$RUST_ITER_PER_NS"
  cat "$records" >> "$NATIVE_RECORDS"

  if [[ "$COSMOS_ACCESS" != "0" && "$COSMOS_ACCESS" != "off" ]]; then
    out="$OUT_DIR/cosmos-block-stm/records-workers-${workers}.jsonl"
    "$BLOCKSTM_BIN" \
      --input "$NATIVE_ACCESS" --output "$out" --workers "$workers" --samples "$SAMPLES" \
      --go-iterations-per-nano "$GO_ITER_PER_NS"
    cat "$out" >> "$COSMOS_RECORDS"
  fi

  if [[ "$COSMOS_WASMD" != "0" && "$COSMOS_WASMD" != "off" ]]; then
    out="$OUT_DIR/cosmos-wasmd-block-stm/records-workers-${workers}.jsonl"
    WASMD_EXTRA_ARGS=()
    if [[ "$RUST_ACG_DEPENDENCY_DIAGNOSTICS" != "0" && "$RUST_ACG_DEPENDENCY_DIAGNOSTICS" != "off" ]]; then
      WASMD_EXTRA_ARGS+=(--symbgraph-rust-dependency-diagnostics)
    fi
    if [[ "$WASMD_INVESTIGATE" != "0" && "$WASMD_INVESTIGATE" != "off" ]]; then
      WASMD_EXTRA_ARGS+=(
        --investigate-overhead
        --diagnostics-output "$OUT_DIR/cosmos-wasmd-block-stm/overhead-workers-${workers}.json"
      )
    fi
    "$WASMD_BLOCKSTM_BIN" \
      --repo-root "$ROOT" \
      --manifest "$EXEC_DIR/execution-manifest.json" \
      --plan "$EXEC_DIR/execution-plan.jsonl" \
      --compute-weights "$WEIGHTS" \
      --symbolic-dir benchmarks/symbolic/native-s3 \
      --output "$out" \
      --workers "$workers" \
      --samples "$SAMPLES" \
      --compute-scale 4 \
      --compute-base-total-ms "$BASE_TOTAL_MS" \
      --go-iterations-per-nano "$WASMD_GO_ITER_PER_NS" \
      --symbgraph-rust-visibility "$RUST_ACG_VISIBILITY" \
      --symbgraph-rust-validation "$RUST_ACG_VALIDATION" \
      --symbgraph-rust-feedback "$RUST_ACG_FEEDBACK" \
      "${RUST_POLICY_ARGS[@]}" \
      "${WASMD_EXTRA_ARGS[@]}"
    cat "$out" >> "$WASMD_RECORDS"
  fi
done

# Optional isolated diagnostics. Each runner is executed in a fresh process so
# CPU, mutex, allocation, and GC state are not inherited from the publication
# benchmark or from another profile. Allocation profiles include before/after
# snapshots suitable for: go tool pprof -diff_base=<before> <after>.
if [[ "$COSMOS_WASMD" != "0" && "$COSMOS_WASMD" != "off" && -n "$WASMD_COMPARE_PPROF_DIR" ]]; then
  mkdir -p "$WASMD_COMPARE_PPROF_DIR"
  for kind in cpu-alloc mutex; do
    for runner in direct-serial outer-cache-serial symbgraph-static; do
      echo "isolated Wasmd profile runner=$runner kind=$kind workers=2 -> $WASMD_COMPARE_PPROF_DIR"
      "$WASMD_BLOCKSTM_BIN" \
        --repo-root "$ROOT" \
        --manifest "$EXEC_DIR/execution-manifest.json" \
        --plan "$EXEC_DIR/execution-plan.jsonl" \
        --compute-weights "$WEIGHTS" \
        --symbolic-dir benchmarks/symbolic/native-s3 \
        --workers 2 \
        --samples 1 \
        --compute-scale 4 \
        --compute-base-total-ms "$BASE_TOTAL_MS" \
        --go-iterations-per-nano "$WASMD_GO_ITER_PER_NS" \
        --profile-only-runner "$runner" \
        --profile-only-kind "$kind" \
        --profile-output-dir "$WASMD_COMPARE_PPROF_DIR"
    done
  done
fi

python3 tools/vegeta/summarize-native-s3-publication-matrix.py \
  --native-records "$NATIVE_RECORDS" \
  --cosmos-records "$COSMOS_RECORDS" \
  --wasmd-records "$WASMD_RECORDS" \
  --output-dir "$OUT_DIR"

python3 - "$OUT_DIR/summary.json" "$SAMPLES" "$COSMOS_ACCESS" "$COSMOS_WASMD" <<'PY'
import json,sys
obj=json.load(open(sys.argv[1])); expected=int(sys.argv[2]); access=sys.argv[3] not in {'0','off'}; wasmd=sys.argv[4] not in {'0','off'}; rows=obj['rows']
assert rows, 'empty publication matrix'
assert all(r['serial_equivalent'] for r in rows), 'state-equivalence failure'
assert all(r['samples']==expected for r in rows), 'incomplete sample count'
assert any(r['strategy']=='static' for r in rows), 'missing deployable static SymbGraph result'
assert any(r['strategy']=='exact-access' for r in rows), 'missing exact-access oracle'
if access: assert any(r['strategy']=='cosmos-block-stm-access-replay' for r in rows), 'missing Cosmos access-replay baseline'
if wasmd:
    required = [
        'cosmos-wasmd-direct-serial',
        'cosmos-wasmd-block-stm',
        'cosmos-wasmd-aria-fb',
        'cosmos-wasmd-symbgraph-rust',
        'cosmos-wasmd-vegeta',
    ]
    by_strategy = {r['strategy']: r for r in rows}
    for strategy in required:
        assert strategy in by_strategy, f'missing Wasmd scheduler row: {strategy}'
        assert 'wasmd-wasmvm' in by_strategy[strategy]['scope'], by_strategy[strategy]['scope']
print('PASS: publication matrix complete, serial-equivalent, and includes native plus five-way Wasmd scheduler rows')
PY
cat "$OUT_DIR/summary.txt"
