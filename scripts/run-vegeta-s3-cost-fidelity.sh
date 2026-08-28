#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
TRACE_DIR="${VEGETA_S3_EXACT_TRACE_DIR:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces}"
OUT_DIR="${VEGETA_S3_COST_FIDELITY_DIR:-$EXEC_DIR/cost-fidelity}"
ALLOWED_MISSING_SOURCE="${VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS:-2}"
for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required input: $p" >&2; exit 2; }
done
[[ -d "$TRACE_DIR" ]] || { echo "missing exact source trace directory: $TRACE_DIR" >&2; exit 2; }
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --bin acg-vegeta-native-s3-executor --release
mkdir -p "$OUT_DIR"
TIMED="$OUT_DIR/native-accesses-timed.jsonl"
runtime/target/release/acg-vegeta-native-s3-executor \
  --repo-root "$ROOT" \
  --manifest "$EXEC_DIR/execution-manifest.json" \
  --plan "$EXEC_DIR/execution-plan.jsonl" \
  --output "$TIMED"
python3 scripts/vegeta/analyze-native-s3-cost-fidelity.py \
  --native-accesses "$TIMED" \
  --source-traces-dir "$TRACE_DIR" \
  --output-dir "$OUT_DIR" \
  --max-missing-source "$ALLOWED_MISSING_SOURCE"
python3 - "$OUT_DIR/summary.json" "$ALLOWED_MISSING_SOURCE" <<'PY'
import json,sys
s=json.load(open(sys.argv[1]))
assert s['matched_transactions'] > 0, 'no matched transactions'
assert s['hash_mismatches'] == 0, f"source/native hash mismatches: {s['hash_mismatches']}"
assert s['missing_native_transactions'] == 0, f"missing native transactions: {s['missing_native_transactions']}"
allowed=int(sys.argv[2])
assert s['missing_source_transactions'] <= allowed, f"missing source transactions: {s['missing_source_transactions']} > allowed {allowed}"
print(f"PASS: source/native transaction cost join is hash-consistent; missing source traces={s['missing_source_transactions']} (allowed={allowed})")
PY
cat "$OUT_DIR/summary.txt"
