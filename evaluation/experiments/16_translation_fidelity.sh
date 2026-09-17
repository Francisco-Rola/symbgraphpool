#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
CORPUS="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/native-execution"
PLAN="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl"
TRACE="$ROOT/benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces"
OUT="$RESULT_ROOT/16-translation-fidelity"
NATIVE="$EXEC/native-accesses.jsonl"
MANIFEST="$EXEC/execution-manifest.json"
EXEC_PLAN="$EXEC/execution-plan.jsonl"
require_file "$CORPUS"; require_file "$PLAN"; require_dir "$TRACE"
require_file "$MANIFEST"; require_file "$EXEC_PLAN"

native_reuse_audit_ready() {
  [[ -s "$NATIVE" ]] || return 1
  python3 - "$NATIVE" <<'PY2'
import json, sys
p=sys.argv[1]
lifecycles=set(); blocks=0
try:
    with open(p, encoding='utf-8') as f:
        for line in f:
            if not line.strip():
                continue
            row=json.loads(line); blocks += 1
            lifecycles.add(row.get('wasm_instance_lifecycle'))
except Exception as e:
    print(f"native access audit unreadable: {e}", file=sys.stderr)
    raise SystemExit(1)
if blocks and lifecycles == {'reuse'}:
    raise SystemExit(0)
print(f"stale native access audit: blocks={blocks} lifecycle={sorted(map(str,lifecycles))}", file=sys.stderr)
raise SystemExit(1)
PY2
}

# native-accesses.jsonl is derived output. Older S3 preparations can predate the
# reuse-lifecycle metadata required by the cost-fidelity analysis. Refresh only
# this audit with the current executor; the prepared workload and source traces
# remain unchanged.
if ! native_reuse_audit_ready; then
  echo "refreshing stale S3 native access/cost audit with current Wasm reuse executor"
  cargo build --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --release
  tmp="$NATIVE.tmp.$$"
  trap 'rm -f "$tmp"' EXIT
  runtime/target/release/acg-vegeta-native-s3-executor \
    --repo-root "$ROOT" \
    --manifest "$MANIFEST" \
    --plan "$EXEC_PLAN" \
    --output "$tmp"
  mv "$tmp" "$NATIVE"
  trap - EXIT
  native_reuse_audit_ready || { echo "refreshed native access audit is not reuse-lifecycle data" >&2; exit 3; }
fi

mkdir -p "$OUT/topology" "$OUT/cost"
python3 tools/vegeta/measure-native-s3-fidelity.py --corpus "$CORPUS" --native-accesses "$NATIVE" --native-plan "$PLAN" --output-dir "$OUT/topology" --dataset vegeta-s3-native
python3 tools/vegeta/analyze-native-s3-cost-fidelity.py --native-accesses "$NATIVE" --source-traces-dir "$TRACE" --output-dir "$OUT/cost" --max-missing-source "${PAPER_EVAL_S3_ALLOWED_MISSING_SOURCE:-2}"
echo "translation fidelity: $OUT"
