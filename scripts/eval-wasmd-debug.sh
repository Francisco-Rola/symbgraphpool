#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Stable S3 regression entrypoint. Pin workload-specific inputs so S1 debug
# environment variables cannot accidentally redirect this command to the S1
# corpus. Worker/sample/build knobs remain available through the normal
# EVAL_WASMD_* variables.
export EVAL_WASMD_DATASET="vegeta-s3-wasmd-blockstm"
export EVAL_WASMD_EXEC_DIR="benchmarks/corpora/vegeta-ethereum/s3/native-execution"
export EVAL_WASMD_TRACE_DIR="benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces"
export EVAL_WASMD_EXACT_NATIVE_ACCESSES="$EVAL_WASMD_EXEC_DIR/native-accesses.jsonl"
export EVAL_WASMD_SYMBOLIC_DIR="benchmarks/symbolic/native-s3"
export EVAL_WASMD_EXACT_ORACLE=1
export EVAL_WASMD_STREAM_PLAN=0
export EVAL_WASMD_ALLOWED_MISSING_SOURCE=2
export EVAL_WASMD_MAX_BLOCKS="${EVAL_WASMD_S3_DEBUG_MAX_BLOCKS:-0}"
export EVAL_WASMD_OUTPUT_DIR="${EVAL_WASMD_S3_DEBUG_OUTPUT_DIR:-benchmark-results/wasmd-debug}"
export EVAL_WASMD_OVERWRITE="${EVAL_WASMD_S3_DEBUG_OVERWRITE:-1}"

# Preserve the earlier S3 state-engine baseline. S1 uses cache=0 + synchronous
# pruning through its own wrapper; S3 regression must not inherit those values.
export EVAL_WASMD_IAVL_CACHE_SIZE="${EVAL_WASMD_S3_IAVL_CACHE_SIZE:-500000}"
export EVAL_WASMD_IAVL_SYNC_PRUNING="${EVAL_WASMD_S3_IAVL_SYNC_PRUNING:-0}"

# Local development profile: on a 6-physical-core host this resolves to 2,4,6
# workers with one sample. Override EVAL_WASMD_WORKERS/EVAL_WASMD_SAMPLES freely.
exec bash scripts/eval-wasmd.sh debug
