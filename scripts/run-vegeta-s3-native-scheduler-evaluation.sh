#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CONFIG="evaluation/vegeta/s3-native-scheduler.v1.json"
PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
SCHED_DIR="${VEGETA_S3_NATIVE_SCHEDULER_DIR:-$EXEC_DIR/native-scheduler-v1}"
FREEZE_DIR="${VEGETA_S3_ARCHETYPE_FREEZE_DIR:-$PLAN_DIR/archetype-freeze}"
TOPOLOGY_VALIDATION="${VEGETA_S3_TOPOLOGY_VALIDATION:-$EXEC_DIR/exact-family-extension-validation/validation.json}"

for path in \
  "$CONFIG" \
  "$EXEC_DIR/execution-manifest.json" \
  "$EXEC_DIR/execution-plan.jsonl" \
  "$TOPOLOGY_VALIDATION"; do
  if [[ ! -s "$path" ]]; then
    echo "missing frozen scheduler input: $path" >&2
    echo "Run scripts/run-vegeta-s3-exact-family-extension-evaluation.sh first if native execution inputs are absent." >&2
    exit 2
  fi
done

readarray -t CFG < <(python3 - "$CONFIG" <<'PY'
import json, sys
c=json.load(open(sys.argv[1]))
for key in ("workers","samples","consensus_cutoff_ms","probability_threshold","cost_bypass_speedup","strategy_order_seed"):
    print(c[key])
PY
)
WORKERS="${CFG[0]}"
SAMPLES="${CFG[1]}"
CUTOFF_MS="${CFG[2]}"
PROBABILITY_THRESHOLD="${CFG[3]}"
COST_BYPASS_SPEEDUP="${CFG[4]}"
ORDER_SEED="${CFG[5]}"

bash scripts/validate-vegeta-s3-native-scheduler.sh
bash scripts/run-vegeta-s3-archetype-freeze.sh

# Rebuild real native contracts and the scheduler binary from the checked-in source used by the
# symbolic profiles. The benchmark consumes the already-frozen execution manifest/plan; it does
# not rerun Ethereum tracing or topology tuning.
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml \
  -p acg-vegeta-native-s3-executor \
  --bin acg-vegeta-native-s3-benchmark \
  --release

mkdir -p "$SCHED_DIR"
RECORDS="$SCHED_DIR/records.jsonl"
rm -f "$RECORDS"

runtime/target/release/acg-vegeta-native-s3-benchmark \
  --repo-root "$ROOT" \
  --manifest "$EXEC_DIR/execution-manifest.json" \
  --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
  --symbolic-dir benchmarks/symbolic/native-s3 \
  --output "$RECORDS" \
  --workers "$WORKERS" \
  --samples "$SAMPLES" \
  --consensus-cutoff-ms "$CUTOFF_MS" \
  --probability-threshold "$PROBABILITY_THRESHOLD" \
  --cost-bypass-speedup "$COST_BYPASS_SPEEDUP" \
  --order-seed "$ORDER_SEED"

python3 scripts/vegeta/summarize-native-s3-scheduler.py \
  --records "$RECORDS" \
  --output-dir "$SCHED_DIR"

python3 scripts/vegeta/validate-native-s3-scheduler-results.py \
  --records "$RECORDS" \
  --config "$CONFIG" \
  --freeze "$FREEZE_DIR/candidate-archetype-freeze.json" \
  --topology-validation "$TOPOLOGY_VALIDATION" \
  --output-dir "$SCHED_DIR"

printf '\nVegeta S3 native scheduler outputs:\n'
printf '  %s/candidate-archetype-freeze.txt\n' "$FREEZE_DIR"
printf '  %s/records.jsonl\n' "$SCHED_DIR"
printf '  %s/summary.txt\n' "$SCHED_DIR"
printf '  %s/summary.csv\n' "$SCHED_DIR"
printf '  %s/per-sample.csv\n' "$SCHED_DIR"
printf '  %s/validation.txt\n' "$SCHED_DIR"
printf '\nExact SLOAD/SSTORE extraction is not rerun by this script.\n'
