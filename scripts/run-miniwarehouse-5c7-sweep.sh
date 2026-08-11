#!/usr/bin/env bash
set -euo pipefail

# Run the Brick-5C.7 MiniWarehouse Wasm block-local-MVCC experiment across a block-size × worker-count matrix.
# Runs are intentionally sequential: concurrent benchmark processes would compete for CPU/cache and
# corrupt wall-clock speedup measurements.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

WAREHOUSES="${1:-${ACG_MW_WAREHOUSES:-4}}"
TOTAL_TXS="${ACG_MW_TOTAL_TXS:-1000}"
BLOCK_INTERVAL_MS="${ACG_MW_BLOCK_INTERVAL_MS:-700}"
CONSENSUS_MS="${ACG_MW_CONSENSUS_MS:-700}"
BLOCK_SIZES="${ACG_MW_SWEEP_BLOCK_SIZES:-25 50 100 200}"
WORKERS="${ACG_MW_SWEEP_WORKERS:-1 2 4 8}"
SEED="${ACG_MW_SEED:-42}"
SYMBOLIC_HARD_SOFTEN_AFTER="${ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER:-8}"

RESULTS_ROOT="${ACG_MW_RESULTS_DIR:-$ROOT_DIR/benchmark-results/miniwarehouse-5c7}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RESULTS_DIR="$RESULTS_ROOT/$RUN_ID"
mkdir -p "$RESULTS_DIR"

if ! rustup target list --installed | grep -qx 'wasm32-unknown-unknown'; then
  echo "Installing wasm32-unknown-unknown target..."
  rustup target add wasm32-unknown-unknown
fi

echo "Building real MiniWarehouse Wasm artifact..."
cargo build \
  --manifest-path benchmarks/Cargo.toml \
  -p acg-benchmark-miniwarehouse \
  --release \
  --target wasm32-unknown-unknown

DEFAULT_WASM="$ROOT_DIR/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_miniwarehouse.wasm"
WASM_PATH="${ACG_MW_WASM:-$DEFAULT_WASM}"
if [[ ! -s "$WASM_PATH" ]]; then
  echo "MiniWarehouse Wasm artifact not found or empty: $WASM_PATH" >&2
  exit 1
fi
WASM_PATH="$(cd "$(dirname "$WASM_PATH")" && pwd)/$(basename "$WASM_PATH")"

cat <<EOF
============================================================
 MiniWarehouse Brick-5C.7 READY-DAG + block-local-MVCC Wasm sweep
============================================================
warehouses:            $WAREHOUSES
fixed districts/wh:    10
fixed customers/dist:  3000
fixed items/wh:        100000
total transactions:    $TOTAL_TXS
block sizes:           $BLOCK_SIZES
workers:               $WORKERS
block interval:        ${BLOCK_INTERVAL_MS} ms
consensus window:      ${CONSENSUS_MS} ms
seed:                  $SEED
symbolic hard soften:  $SYMBOLIC_HARD_SOFTEN_AFTER concrete observations
Wasm:                  $WASM_PATH
results:               $RESULTS_DIR
============================================================
EOF

SUMMARY_FILE="$RESULTS_DIR/summary.txt"
: > "$SUMMARY_FILE"

for block_size in $BLOCK_SIZES; do
  for worker_count in $WORKERS; do
    LOG_FILE="$RESULTS_DIR/block-${block_size}_workers-${worker_count}.log"
    echo
    echo "================ BLOCK=$block_size WORKERS=$worker_count ================"

    ACG_MW_WASM="$WASM_PATH" \
    ACG_MW_WAREHOUSES="$WAREHOUSES" \
    ACG_MW_TOTAL_TXS="$TOTAL_TXS" \
    ACG_MW_BLOCK_SIZE="$block_size" \
    ACG_MW_BLOCK_INTERVAL_MS="$BLOCK_INTERVAL_MS" \
    ACG_MW_CONSENSUS_MS="$CONSENSUS_MS" \
    ACG_MW_TOTAL_WORKERS="$worker_count" \
    ACG_MW_SEED="$SEED" \
    ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER="$SYMBOLIC_HARD_SOFTEN_AFTER" \
    cargo test \
      --release \
      --manifest-path runtime/Cargo.toml \
      -p acg-miniwarehouse-workload \
      --test parallelism_metrics \
      -- \
      --nocapture --test-threads=1 \
      2>&1 | tee "$LOG_FILE"

    {
      echo "================ BLOCK=$block_size WORKERS=$worker_count ================"
      grep -E \
        '^(  edge density:|  predicate True:|  predicate Unknown:|  Low / Soft / Hard:|  waves:|  average wave width:|  execution dependencies:|  hard execution dependencies:|  theoretical speedup:|  REALIZED preexec speedup:|  deadline hit rate:|  preexecution feedback:|  post-replay feedback:|  reused results:|  invalidated results:|  replayed transactions:|  reuse rate:|  invalidation rate:|  replay rate:|  SERIAL baseline:|  ADAPTIVE total:|  POST-CONSENSUS SPEEDUP:|  timing accounting gap:|  invalidated transactions:|  concrete validation conflicts:|  txs with unguarded predecessor:|  txs with cascade candidate:|  txs with unattributed conflict:|  guarded by execution dependency:|  unguarded predecessor conflicts:|  cascade: guarded predecessor replayed:|  guarded\+reused unexplained:|  unattributed conflicts:|  writer level relation same/earlier/later:)' \
        "$LOG_FILE" || true
      echo
    } >> "$SUMMARY_FILE"
  done
done

echo
echo "Sweep complete."
echo "Full logs: $RESULTS_DIR"
echo "Condensed summary: $SUMMARY_FILE"
