#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
OUT_DIR="${VEGETA_S3_REPLAY_SCALING_DIR:-$EXEC_DIR/replay-scaling}"
WORKERS_LIST="${VEGETA_S3_REPLAY_WORKERS:-1,2,4,6,8,16}"
SAMPLES="${VEGETA_S3_REPLAY_SAMPLES:-3}"
CUTOFF_MS="${VEGETA_S3_REPLAY_CUTOFF_MS:-5000}"
ORDER_SEED="${VEGETA_S3_REPLAY_ORDER_SEED:-2026082501}"
for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --bin acg-vegeta-native-s3-benchmark --release
mkdir -p "$OUT_DIR"
RECORDS="$OUT_DIR/records.jsonl"
: > "$RECORDS"
IFS=',' read -r -a WORKERS <<< "$WORKERS_LIST"
for workers in "${WORKERS[@]}"; do
  tmp="$OUT_DIR/records-workers-${workers}.jsonl"
  runtime/target/release/acg-vegeta-native-s3-benchmark \
    --repo-root "$ROOT" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --symbolic-dir benchmarks/symbolic/native-s3 \
    --output "$tmp" \
    --workers "$workers" \
    --samples "$SAMPLES" \
    --consensus-cutoff-ms "$CUTOFF_MS" \
    --order-seed "$ORDER_SEED" \
    --strategies serial,exact-direct,exact-access
  cat "$tmp" >> "$RECORDS"
done
python3 tools/vegeta/summarize-native-s3-replay-scaling.py --records "$RECORDS" --output-dir "$OUT_DIR"
python3 - "$OUT_DIR/summary.json" <<'PY'
import json,sys
rows=json.load(open(sys.argv[1]))
assert rows, 'empty replay scaling summary'
assert all(r['serial_equivalent'] for r in rows), 'state-equivalence failure in replay scaling run'
direct=[r for r in rows if r['strategy']=='exact-direct']
assert direct, 'missing exact-direct rows'
print('PASS: direct DAG replay scaling completed with serial-equivalent state')
PY
cat "$OUT_DIR/summary.txt"
