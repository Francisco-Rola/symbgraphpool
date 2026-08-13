#!/usr/bin/env bash
set -euo pipefail

WAREHOUSES="${ACG_MW_WAREHOUSES:-16}"
BLOCK_SIZE="${ACG_MW_BLOCK_SIZE:-200}"
TOTAL_TXS="${ACG_MW_TOTAL_TXS:-1000}"
REPEATS="${ACG_MW_DIAG_REPEATS:-1}"
PHYSICAL_CORES="${ACG_PHYSICAL_CORES:-6}"
WORKERS_LIST="${ACG_MW_DIAG_WORKERS:-1 2 4 6}"
SOFTEN_AFTER="${ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER:-8}"

for workers_requested in $WORKERS_LIST; do
  if (( workers_requested > PHYSICAL_CORES )); then
    echo "refusing worker count $workers_requested: physical-core budget is $PHYSICAL_CORES" >&2
    exit 2
  fi
done
BLOCK_INTERVAL_MS="${ACG_MW_BLOCK_INTERVAL_MS:-700}"
CONSENSUS_MS="${ACG_MW_CONSENSUS_MS:-700}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${ACG_MW_DIAG_OUT_DIR:-benchmark-results/miniwarehouse-5c7-mvcc-diagnostics/${STAMP}}"
mkdir -p "$OUT_DIR"

summary="$OUT_DIR/summary.txt"
: > "$summary"

for workers in $WORKERS_LIST; do
  for repeat in $(seq 1 "$REPEATS"); do
    log="$OUT_DIR/workers-${workers}_repeat-${repeat}.log"
    echo "=== workers=$workers repeat=$repeat ===" | tee -a "$summary"
    ACG_MW_WAREHOUSES="$WAREHOUSES" \
    ACG_MW_TOTAL_TXS="$TOTAL_TXS" \
    ACG_MW_BLOCK_SIZE="$BLOCK_SIZE" \
    ACG_MW_TOTAL_WORKERS="$workers" \
    ACG_MW_BLOCK_INTERVAL_MS="$BLOCK_INTERVAL_MS" \
    ACG_MW_CONSENSUS_MS="$CONSENSUS_MS" \
    ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER="$SOFTEN_AFTER" \
    cargo test \
      --release \
      --manifest-path runtime/Cargo.toml \
      -p acg-miniwarehouse-workload \
      --test parallelism_metrics \
      -- \
      --nocapture --test-threads=1 | tee "$log"

    grep -E \
      'serial-equivalent work:|actual parallel preexecute:|REALIZED preexec speedup:|dependency-DAG lower bound:|engine dependency total:|dependency plan/setup:|worker phase wall:|aggregate ready/lock wait:|aggregate visibility capture:|aggregate contract\+receipt:|aggregate publish/unblock:|aggregate measured stages:|serial tx work sum:|speculative contract cost / serial:|effective contract concurrency:|worker busy capacity:|worker ready/wait capacity:|validator/base-snapshot wrapper:|executor coordinator residual:|max in-flight transactions:|per-tx full-world deep copies:|historical write-set replays:|visibility masks captured:|visibility words copied:|visibility words / tx:|MVCC storage versions published:|MVCC balance versions published:|MVCC contract versions published:|total MVCC versions published:|aggregate request execution:|aggregate receipt finalization:|outer contract timing residual:|aggregate pre-contract setup:|aggregate response processing:|aggregate outcome assembly:|aggregate backend construction:|aggregate Wasm instance acquire:|aggregate Wasm entrypoint:|aggregate Wasm recycle:|aggregate Wasm runtime total:|aggregate host storage callbacks:|aggregate host queries:|aggregate tx-lock wait in host:|aggregate MVCC storage point:|aggregate MVCC storage range:|aggregate MVCC balance point:|aggregate MVCC all balances:|aggregate MVCC contract lookup:|aggregate MVCC lock wait:|aggregate MVCC publish:|Wasm instance acquires:|Wasm entrypoint calls:|Wasm instance recycles:|Wasm cache pinned/mem/fs/miss:|host storage get/scan/next/set/remove:|host storage ops / tx:|host query calls:|MVCC storage point reads:|MVCC point version hits:|MVCC point base fallbacks:|MVCC storage range reads:|MVCC balance/all/contract reads:|receipt access records:|receipt read dependencies:|receipt storage/balance/contract writes:|reuse rate:|replay rate:|POST-CONSENSUS SPEEDUP:' \
      "$log" | tee -a "$summary"
    echo | tee -a "$summary"
  done
done

echo "Logs: $OUT_DIR"
echo "Summary: $summary"
