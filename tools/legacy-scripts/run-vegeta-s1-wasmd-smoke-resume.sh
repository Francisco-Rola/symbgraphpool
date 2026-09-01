#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${EVAL_WASMD_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
OUT_DIR="${EVAL_WASMD_OUTPUT_DIR:-benchmark-results/wasmd-s1-smoke}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing prepared S1 input: $p" >&2; exit 2; }
done

# Reuse only a smoke result produced with the memory-safe isolated-strategy mode.
# Older S1 matrix runs kept seven 325+ MB Wasmd states live at once and can contain
# multi-minute GC/swap stalls that contaminate the measured strategy wall times.
ENV_FILE="$OUT_DIR/environment.txt"
COMPLETE=0
if [[ -s "$OUT_DIR/summary/summary.txt" && -s "$OUT_DIR/summary/summary.csv" && -s "$OUT_DIR/records.jsonl" ]]; then
  COMPLETE=1
fi
if [[ "${EVAL_WASMD_OVERWRITE:-0}" != "1" && "$COMPLETE" == "1" ]] \
   && [[ -s "$ENV_FILE" ]] \
   && grep -qx 'isolate_strategies=1' "$ENV_FILE"; then
  echo "PASS: existing isolated Vegeta S1 Wasmd smoke campaign is complete; reusing $OUT_DIR"
  echo "summary: $OUT_DIR/summary/summary.txt"
  exit 0
fi
if [[ "$COMPLETE" == "1" ]]; then
  echo "existing S1 smoke is legacy/non-isolated or overwrite was requested; replacing benchmark records"
  export EVAL_WASMD_OVERWRITE=1
fi

export EVAL_WASMD_EXEC_DIR="$EXEC_DIR"
export EVAL_WASMD_OUTPUT_DIR="$OUT_DIR"

# The first smoke run has to build the Wasmd scheduler evaluation binary.  If a previous
# attempt already reached that build, reuse it on retry instead of rebuilding Cargo/Go.
BIN="$OUT_DIR/bin/wasmd-scheduler-eval"
GO_MAIN="benchmarks/cosmos-wasmd-blockstm-s3/main.go"
if [[ -x "$BIN" && ! "$GO_MAIN" -nt "$BIN" ]]; then
  echo "reusing cached Wasmd scheduler evaluation binary"
  export EVAL_WASMD_BUILD=0
  # A failed/partial campaign can leave records.jsonl behind. It is safe to replace those
  # diagnostic smoke outputs while preserving the cached binary.
  export EVAL_WASMD_OVERWRITE=1
else
  if [[ -x "$BIN" ]]; then
    echo "cached Wasmd scheduler evaluation binary is stale; rebuilding once"
  else
    echo "Wasmd scheduler evaluation binary not cached; first smoke run will build it"
  fi
  export EVAL_WASMD_BUILD=1
fi

# The S1 wrapper fixes the smoke domain to 101 blocks, one sample, two workers, streamed plan,
# and no exact-oracle dependency on post-execution native-access traces.
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh smoke

echo "PASS: Vegeta S1 101-block Wasmd scheduler smoke completed"
echo "summary: $OUT_DIR/summary/summary.txt"
