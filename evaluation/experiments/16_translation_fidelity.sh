#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
CORPUS="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"
EXEC="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/native-execution"
PLAN="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl"
TRACE="$ROOT/benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces"
OUT="$RESULT_ROOT/16-translation-fidelity"
require_file "$CORPUS"; require_file "$EXEC/native-accesses.jsonl"; require_file "$PLAN"; require_dir "$TRACE"
mkdir -p "$OUT/topology" "$OUT/cost"
python3 tools/vegeta/measure-native-s3-fidelity.py --corpus "$CORPUS" --native-accesses "$EXEC/native-accesses.jsonl" --native-plan "$PLAN" --output-dir "$OUT/topology" --dataset vegeta-s3-native
python3 tools/vegeta/analyze-native-s3-cost-fidelity.py --native-accesses "$EXEC/native-accesses.jsonl" --source-traces-dir "$TRACE" --output-dir "$OUT/cost" --max-missing-source "${PAPER_EVAL_S3_ALLOWED_MISSING_SOURCE:-2}"
echo "translation fidelity: $OUT"
