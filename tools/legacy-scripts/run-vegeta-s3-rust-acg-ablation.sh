#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_RUST_ACG_ABLATION_DIR:-$EXEC_DIR/rust-acg-ablation}"
MODE="${VEGETA_S3_RUST_ACG_ABLATION_MODE:-debug}"
case "$MODE" in
  smoke) DEFAULT_WORKERS="2"; DEFAULT_SAMPLES="1" ;;
  debug) DEFAULT_WORKERS="2,4"; DEFAULT_SAMPLES="1" ;;
  paper) DEFAULT_WORKERS="1,2,4,8,16"; DEFAULT_SAMPLES="5" ;;
  *) echo "unknown VEGETA_S3_RUST_ACG_ABLATION_MODE=$MODE (use smoke|debug|paper)" >&2; exit 2 ;;
esac
WORKERS_LIST="${VEGETA_S3_RUST_ACG_ABLATION_WORKERS:-$DEFAULT_WORKERS}"
SAMPLES="${VEGETA_S3_RUST_ACG_ABLATION_SAMPLES:-$DEFAULT_SAMPLES}"
BASE_TOTAL_MS="${VEGETA_S3_COMPUTE_BASE_TOTAL_MS:-1000}"
GO_TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
BUILD="${VEGETA_S3_RUST_ACG_ABLATION_BUILD:-1}"
VARIANTS="${VEGETA_S3_RUST_ACG_ABLATION_VARIANTS:-legacy,mvcc,mvcc-indexed,optimized}"
BIN="${VEGETA_S3_RUST_ACG_ABLATION_BIN:-${VEGETA_S3_WASMD_SYMBGRAPH_BIN:-/tmp/vegeta-s3-wasmd-symbgraph-rust}}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing exact source trace directory: $TRACE_DIR" >&2; exit 2; }
mkdir -p "$OUT_DIR"

WEIGHTS="$OUT_DIR/compute-weights.jsonl"
if [[ ! -s "$WEIGHTS" ]]; then
  python3 tools/vegeta/build-native-s3-compute-weights.py \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --source-traces-dir "$TRACE_DIR" \
    --output "$WEIGHTS" \
    --summary "$OUT_DIR/compute-weights-summary.json" \
    --max-missing-source "${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"
fi

if [[ "$BUILD" != "0" && "$BUILD" != "off" ]]; then
  echo "[build] native S3 Wasm artifacts"
  cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
  echo "[build] Rust ACG FFI + Go Wasmd harness + integration tests"
  VEGETA_S3_WASMD_SYMBGRAPH_BIN="$BIN" \
    VEGETA_S3_COSMOS_GO_TOOLCHAIN="$GO_TOOLCHAIN" \
    bash tools/legacy-scripts/build-wasmd-symbgraph-bridge.sh
else
  [[ -x "$BIN" ]] || { echo "BUILD=0 but benchmark binary is missing/not executable: $BIN" >&2; exit 3; }
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
echo "Rust-ACG ablation mode=$MODE workers=$WORKERS_LIST samples=$SAMPLES go-iter/ns=$GO_ITER_PER_NS"

variant_options() {
  case "$1" in
    legacy)       echo "materialized scan all-pairs" ;;
    mvcc)         echo "mvcc scan all-pairs" ;;
    mvcc-indexed) echo "mvcc indexed all-pairs" ;;
    optimized)    echo "mvcc indexed profile" ;;
    *) echo "unknown ablation variant: $1" >&2; return 2 ;;
  esac
}

IFS=',' read -r -a VARIANT_ARRAY <<< "$VARIANTS"
IFS=',' read -r -a WORKER_ARRAY <<< "$WORKERS_LIST"
for variant in "${VARIANT_ARRAY[@]}"; do
  read -r visibility validation feedback <<< "$(variant_options "$variant")"
  variant_dir="$OUT_DIR/$variant"
  mkdir -p "$variant_dir"
  records="$variant_dir/records.jsonl"
  : > "$records"
  cat > "$variant_dir/config.json" <<JSON
{
  "variant": "$variant",
  "visibility": "$visibility",
  "validation": "$validation",
  "feedback": "$feedback",
  "workers": "$WORKERS_LIST",
  "samples": $SAMPLES,
  "go_iterations_per_nano": $GO_ITER_PER_NS
}
JSON
  for workers in "${WORKER_ARRAY[@]}"; do
    echo "=== Rust-ACG ablation variant=$variant workers=$workers samples=$SAMPLES ==="
    out="$variant_dir/records-workers-${workers}.jsonl"
    "$BIN" \
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
      --go-iterations-per-nano "$GO_ITER_PER_NS" \
      --symbgraph-rust-visibility "$visibility" \
      --symbgraph-rust-validation "$validation" \
      --symbgraph-rust-feedback "$feedback"
    validate_graph_metric_schema "$out"
    cat "$out" >> "$records"
  done

done

python3 - "$OUT_DIR" "$SAMPLES" <<'PY'
import json, pathlib, sys
root=pathlib.Path(sys.argv[1]); expected=int(sys.argv[2])
for variant in root.iterdir():
    p=variant/'records.jsonl'
    if not p.exists(): continue
    rows=[json.loads(line) for line in p.read_text().splitlines() if line.strip()]
    assert rows, f'empty records for {variant.name}'
    assert all(r['serial_equivalent'] for r in rows), f'state mismatch in {variant.name}'
    symb=[r for r in rows if r['strategy']=='cosmos-wasmd-symbgraph-rust']
    assert symb, f'missing Rust-ACG rows in {variant.name}'
    by_workers={}
    for r in symb: by_workers.setdefault(r['workers'],set()).add(r['sample'])
    assert all(len(samples)==expected for samples in by_workers.values()), f'incomplete samples in {variant.name}: {by_workers}'
print('PASS: all Rust-ACG ablation variants are serial-equivalent and complete')
PY

python3 tools/vegeta/summarize-wasmd-rust-acg-ablation.py \
  --input-root "$OUT_DIR" \
  --output-dir "$OUT_DIR/summary"

echo
cat "$OUT_DIR/summary/ablation.txt"
echo "CSV:  $OUT_DIR/summary/ablation.csv"
echo "JSON: $OUT_DIR/summary/ablation.json"
