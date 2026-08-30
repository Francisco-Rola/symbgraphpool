#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_RUST_ACG_RISK_SWEEP_DIR:-$EXEC_DIR/rust-acg-risk-sweep}"
MODE="${VEGETA_S3_RUST_ACG_RISK_SWEEP_MODE:-debug}"
PHYSICAL_CORES="${VEGETA_S3_PHYSICAL_CORES:-}"
if [[ -z "$PHYSICAL_CORES" ]] && command -v lscpu >/dev/null 2>&1; then
  PHYSICAL_CORES="$(lscpu -p=Core,Socket 2>/dev/null | grep -v '^#' | sort -u | wc -l | tr -d ' ')"
fi
PHYSICAL_CORES="${PHYSICAL_CORES:-$(nproc)}"
LOGICAL_CPUS="$(nproc)"
case "$MODE" in
  smoke) DEFAULT_WORKERS="2"; DEFAULT_SAMPLES="1" ;;
  debug)
    if (( PHYSICAL_CORES >= 6 )); then DEFAULT_WORKERS="2,4,6"; elif (( PHYSICAL_CORES >= 4 )); then DEFAULT_WORKERS="2,4"; else DEFAULT_WORKERS="2"; fi
    DEFAULT_SAMPLES="1" ;;
  paper)
    if (( PHYSICAL_CORES >= 6 )); then DEFAULT_WORKERS="2,4,6"; elif (( PHYSICAL_CORES >= 4 )); then DEFAULT_WORKERS="2,4"; else DEFAULT_WORKERS="2"; fi
    DEFAULT_SAMPLES="5" ;;
  *) echo "unknown mode=$MODE (smoke|debug|paper)" >&2; exit 2 ;;
esac
WORKERS_LIST="${VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS:-$DEFAULT_WORKERS}"
SAMPLES="${VEGETA_S3_RUST_ACG_RISK_SWEEP_SAMPLES:-$DEFAULT_SAMPLES}"
POLICIES="${VEGETA_S3_RUST_ACG_RISK_SWEEP_POLICIES:-default,thresholds-only,aggressive-no-exploration,moderate+exploration,aggressive}"
BASE_TOTAL_MS="${VEGETA_S3_COMPUTE_BASE_TOTAL_MS:-1000}"
GO_TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
BUILD="${VEGETA_S3_RUST_ACG_RISK_SWEEP_BUILD:-1}"
BIN="${VEGETA_S3_RUST_ACG_RISK_SWEEP_BIN:-${VEGETA_S3_WASMD_SYMBGRAPH_BIN:-/tmp/vegeta-s3-wasmd-symbgraph-rust}}"
mkdir -p "$OUT_DIR"
cat > "$OUT_DIR/hardware.txt" <<EOF
logical_cpus=$LOGICAL_CPUS
physical_cores=$PHYSICAL_CORES
workers=$WORKERS_LIST
note=WSL/SMT-aware defaults stop at physical cores; set VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS=2,4,6,8,12 for an explicit SMT/oversubscription experiment.
EOF
WEIGHTS="$OUT_DIR/compute-weights.jsonl"
if [[ ! -s "$WEIGHTS" ]]; then
  python3 tools/vegeta/build-native-s3-compute-weights.py --execution-plan "$EXEC_DIR/execution-plan.jsonl" --source-traces-dir "$TRACE_DIR" --output "$WEIGHTS" --summary "$OUT_DIR/compute-weights-summary.json" --max-missing-source "${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"
fi
if [[ "$BUILD" != "0" && "$BUILD" != "off" ]]; then
  cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
  VEGETA_S3_WASMD_SYMBGRAPH_BIN="$BIN" VEGETA_S3_COSMOS_GO_TOOLCHAIN="$GO_TOOLCHAIN" bash tools/legacy-scripts/build-wasmd-symbgraph-bridge.sh
else
  [[ -x "$BIN" ]] || { echo "missing binary: $BIN" >&2; exit 3; }
fi
GO_ITER_PER_NS="${VEGETA_S3_WASMD_BLOCKSTM_GO_ITERATIONS_PER_NANO:-$($BIN --calibrate-only)}"
validate_graph_metric_schema() {
  local records="$1"
  python3 - "$records" <<'PYMETRICS'
import json, sys
path = sys.argv[1]
required = (
    'symb_physical_candidate_edges',
    'symb_logical_candidate_edges',
    'symb_compact_candidate_groups',
    'symb_parent_dependencies_before_reduction',
    'symb_parent_dependencies_elided_reduction',
)
rows = []
with open(path, encoding='utf-8') as fh:
    for line in fh:
        if not line.strip():
            continue
        row = json.loads(line)
        if row.get('strategy') == 'cosmos-wasmd-symbgraph-rust':
            rows.append(row)
if not rows:
    raise SystemExit(f'{path}: missing cosmos-wasmd-symbgraph-rust records')
missing = [key for key in required if any(key not in row for row in rows)]
if missing:
    raise SystemExit(
        f'{path}: benchmark binary is stale or graph metrics are not propagated; '
        f'missing fields: {", ".join(missing)}. Rebuild with tools/legacy-scripts/build-wasmd-symbgraph-bridge.sh.'
    )
if not any(row['symb_logical_candidate_edges'] > 0 for row in rows):
    raise SystemExit(f'{path}: graph metric schema is present but logical candidate edges are zero for every block')
if not any(row['symb_physical_candidate_edges'] > 0 for row in rows):
    raise SystemExit(f'{path}: graph metric schema is present but physical candidate edges are zero for every block')
PYMETRICS
}
policy_args() {
  POLICY_ARGS=()
  case "$1" in
    default) ;;
    # Legacy names remain accepted for reproducing the first sweep.
    risk40) POLICY_ARGS+=(--symbgraph-rust-risk-budget 0.40) ;;
    risk60) POLICY_ARGS+=(--symbgraph-rust-risk-budget 0.60) ;;
    soften4) POLICY_ARGS+=(--symbgraph-rust-independence-before-softening 4) ;;
    moderate) POLICY_ARGS+=(--symbgraph-rust-soft-threshold 0.30 --symbgraph-rust-hard-threshold 0.90 --symbgraph-rust-risk-budget 0.50 --symbgraph-rust-independence-before-softening 4 --symbgraph-rust-softening-min-confidence 0.15) ;;
    thresholds-only) POLICY_ARGS+=(--symbgraph-rust-soft-threshold 0.40 --symbgraph-rust-hard-threshold 0.95) ;;
    aggressive-no-exploration) POLICY_ARGS+=(--symbgraph-rust-soft-threshold 0.40 --symbgraph-rust-hard-threshold 0.95 --symbgraph-rust-risk-budget 0.70 --symbgraph-rust-independence-before-softening 2 --symbgraph-rust-softening-min-confidence 0.10) ;;
    moderate+exploration) POLICY_ARGS+=(--symbgraph-rust-soft-threshold 0.30 --symbgraph-rust-hard-threshold 0.90 --symbgraph-rust-risk-budget 0.50 --symbgraph-rust-independence-before-softening 4 --symbgraph-rust-softening-min-confidence 0.15 --symbgraph-rust-exploration-rate 0.10 --symbgraph-rust-exploration-risk-budget 0.90 --symbgraph-rust-exploration-min-uncertainty 0.20 --symbgraph-rust-exploration-max-transactions 8) ;;
    aggressive) POLICY_ARGS+=(--symbgraph-rust-soft-threshold 0.40 --symbgraph-rust-hard-threshold 0.95 --symbgraph-rust-risk-budget 0.70 --symbgraph-rust-independence-before-softening 2 --symbgraph-rust-softening-min-confidence 0.10 --symbgraph-rust-exploration-rate 0.10 --symbgraph-rust-exploration-risk-budget 0.90 --symbgraph-rust-exploration-min-uncertainty 0.20 --symbgraph-rust-exploration-max-transactions 8) ;;
    *) echo "unknown risk policy $1" >&2; return 2 ;;
  esac
}
echo "Rust-ACG risk sweep mode=$MODE physical=$PHYSICAL_CORES logical=$LOGICAL_CPUS workers=$WORKERS_LIST samples=$SAMPLES policies=$POLICIES"
IFS=',' read -r -a POLICY_ARRAY <<< "$POLICIES"
IFS=',' read -r -a WORKER_ARRAY <<< "$WORKERS_LIST"
for policy in "${POLICY_ARRAY[@]}"; do
  policy_args "$policy"
  dir="$OUT_DIR/$policy"; mkdir -p "$dir"; : > "$dir/records.jsonl"
  printf '%q ' "${POLICY_ARGS[@]}" > "$dir/policy-args.txt"; printf '\n' >> "$dir/policy-args.txt"
  for workers in "${WORKER_ARRAY[@]}"; do
    echo "=== risk policy=$policy workers=$workers samples=$SAMPLES ==="
    out="$dir/records-workers-${workers}.jsonl"
    "$BIN" --repo-root "$ROOT" --manifest "$EXEC_DIR/execution-manifest.json" --plan "$EXEC_DIR/execution-plan.jsonl" --compute-weights "$WEIGHTS" --symbolic-dir benchmarks/symbolic/native-s3 --output "$out" --workers "$workers" --samples "$SAMPLES" --compute-scale 4 --compute-base-total-ms "$BASE_TOTAL_MS" --go-iterations-per-nano "$GO_ITER_PER_NS" --symbgraph-rust-visibility mvcc --symbgraph-rust-validation indexed --symbgraph-rust-feedback profile --symbgraph-rust-dependency-diagnostics --rust-acg-only "${POLICY_ARGS[@]}"
    validate_graph_metric_schema "$out"
    cat "$out" >> "$dir/records.jsonl"
  done
  python3 tools/vegeta/summarize-wasmd-rust-acg-bottlenecks.py --input "$dir/records.jsonl" --output-dir "$dir/bottlenecks" --top "${VEGETA_S3_RUST_ACG_WORST_BLOCKS:-10}" >/dev/null
done
python3 tools/vegeta/summarize-wasmd-rust-acg-risk-sweep.py --input-root "$OUT_DIR" --output-dir "$OUT_DIR/summary"
echo "Worst-block reports: $OUT_DIR/<policy>/bottlenecks/worst-blocks.txt"
