#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
MODE="${1:-debug}"

export EVAL_WASMD_EXEC_DIR="${EVAL_WASMD_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
export EVAL_WASMD_SYMBOLIC_DIR="${EVAL_WASMD_SYMBOLIC_DIR:-benchmarks/symbolic/native-s3}"
export EVAL_WASMD_EXACT_ORACLE=0
export EVAL_WASMD_STREAM_PLAN=1
# S1 full-domain tuning: MemDB already holds the state, so avoid retaining a
# second 500k-node IAVL LRU and keep pruning inside Commit instead of allowing
# background pruning to steal CPU from the next measured block.
export EVAL_WASMD_IAVL_CACHE_SIZE="${EVAL_WASMD_IAVL_CACHE_SIZE:-${VEGETA_WASMD_IAVL_CACHE_SIZE:-0}}"
export EVAL_WASMD_IAVL_SYNC_PRUNING="${EVAL_WASMD_IAVL_SYNC_PRUNING:-${VEGETA_WASMD_IAVL_SYNC_PRUNING:-1}}"
export EVAL_WASMD_SETUP_CHECK="${EVAL_WASMD_SETUP_CHECK:-0}"
export EVAL_WASMD_ISOLATE_STRATEGIES="${EVAL_WASMD_ISOLATE_STRATEGIES:-1}"
if [[ -z "${EVAL_WASMD_MAX_BLOCKS+x}" ]]; then
  if [[ "$MODE" == "smoke" ]]; then EVAL_WASMD_MAX_BLOCKS=101; else EVAL_WASMD_MAX_BLOCKS=0; fi
fi
export EVAL_WASMD_MAX_BLOCKS
export EVAL_WASMD_DATASET="${EVAL_WASMD_DATASET:-vegeta-s1-wasmd}"
export EVAL_WASMD_ALLOWED_MISSING_SOURCE=0
export EVAL_WASMD_OUTPUT_DIR="${EVAL_WASMD_OUTPUT_DIR:-benchmark-results/wasmd-s1-${MODE}}"

bash evaluation/lib/run_wasmd_campaign.sh "$MODE"
