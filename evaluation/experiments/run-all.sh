#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
for exp in 00_validate 01_s1_headline 02_s4_headline 03_s3_breakdown 04_native_apps 05_conflictlab_upper_bound 06_conflictlab_contention 07_conflictlab_prediction 08_conflictlab_adaptation 09_s3_acg_ablation 10_conflictlab_block_size 11_conflictlab_consensus 12_conflictlab_semantics 13_conflictlab_compaction 14_consensus_window_sensitivity; do
  echo; echo "===== $exp ====="
  bash "$ROOT/evaluation/experiments/$exp.sh"
done
python3 "$ROOT/evaluation/plots/plot_all.py"
