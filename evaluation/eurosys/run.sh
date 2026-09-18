#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
PROFILE="${PAPER_EVAL_PROFILE:-debug}"
CORES="$(bash -lc 'source evaluation/lib/common.sh >/dev/null 2>&1; physical_cores')"
TAG="${PAPER_EVAL_MACHINE_TAG:-$(hostname -s)-${CORES}c}"
if [[ -z "${PAPER_EVAL_RESULT_ROOT:-}" ]]; then export PAPER_EVAL_RESULT_ROOT="$ROOT/benchmark-results/eurosys/$TAG"; fi
export PAPER_EVAL_RESOURCE_ACCOUNTING="${PAPER_EVAL_RESOURCE_ACCOUNTING:-1}"
export PAPER_EVAL_REQUIRE_S4="${PAPER_EVAL_REQUIRE_S4:-1}"
source "$ROOT/evaluation/lib/common.sh"
mkdir -p "$RESULT_ROOT/eurosys-summary" "$RESULT_ROOT/paper"
python3 evaluation/eurosys/capture_machine.py --output "$RESULT_ROOT/machine.json" --tag "$TAG" --profile "$PROFILE" --workers "$WORKERS" --samples "$SAMPLES"

CORE=(
  00_validate.sh
  01_s1_headline.sh
  02_s4_headline.sh
  03_s3_breakdown.sh
  04_native_apps.sh
  05_conflictlab_upper_bound.sh
  06_conflictlab_contention.sh
  07_conflictlab_prediction.sh
  08_conflictlab_adaptation.sh
  09_s3_acg_ablation.sh
  10_conflictlab_block_size.sh
  11_conflictlab_consensus.sh
  12_conflictlab_semantics.sh
  13_conflictlab_compaction.sh
  14_consensus_window_sensitivity.sh
  15_compute_sensitivity.sh
  16_translation_fidelity.sh
)
for script in "${CORE[@]}"; do
  echo
  echo "===== EuroSys evaluation: $script ====="
  bash "$ROOT/evaluation/experiments/$script"
done

RUN_IAVL="${PAPER_EVAL_RUN_IAVL_SENSITIVITY:-}"
if [[ -z "$RUN_IAVL" ]]; then [[ "$PROFILE" == paper ]] && RUN_IAVL=1 || RUN_IAVL=0; fi
if [[ "$RUN_IAVL" == 1 ]]; then bash "$ROOT/evaluation/experiments/17_iavl_sensitivity.sh"; fi

python3 evaluation/eurosys/summarize_headline_records.py \
  --dataset "S1=$RESULT_ROOT/01-s1/records.jsonl" \
  --dataset "S4=$RESULT_ROOT/02-s4/records.jsonl" \
  --consensus-window-ms "$CANONICAL_CONSENSUS_WINDOW_MS" \
  --output-dir "$RESULT_ROOT/eurosys-summary"
python3 evaluation/eurosys/build_tables.py --result-root "$RESULT_ROOT" --output-dir "$RESULT_ROOT/paper"
python3 evaluation/eurosys/plot_main.py --result-root "$RESULT_ROOT" --output-dir "$RESULT_ROOT/paper" --strict

cat > "$RESULT_ROOT/paper/README.txt" <<EOF
EuroSys publication bundle
machine: $TAG
profile: $PROFILE
workers: $WORKERS
samples: $SAMPLES
canonical modeled consensus window: $CANONICAL_CONSENSUS_WINDOW_MS ms

Main figures:
  fig01-real-workload-headline.pdf
  fig02-scalability-and-tail-distribution.pdf
  fig03-generality-contention-ceiling.pdf
  fig04-cost-and-overheads.pdf
  fig05-prediction-and-adaptation.pdf
  fig06-consensus-robustness.pdf
Main tables:
  table1-workloads-fidelity.csv
  table2-semantics-correctness.csv
Supplementary outputs are emitted when the corresponding experiment is present.
EOF

echo
echo "PASS: EuroSys evaluation bundle: $RESULT_ROOT/paper"
