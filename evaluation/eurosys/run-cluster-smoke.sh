#!/usr/bin/env bash
# Short cluster smoke: validate the node, then exercise the real S1/S4 paths,
# exact-oracle path, zero-conflict ceiling, and semantic correctness matrix.

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/evaluation/eurosys/cluster-common.sh"

"$ROOT/evaluation/eurosys/cluster-preflight.sh"

CORES="$(cluster_affinity_physical_cores)"
TAG="${PAPER_EVAL_MACHINE_TAG:-$(cluster_default_tag "$CORES")}"
if (( CORES > 1 )); then WORKERS="1,$CORES"; else WORKERS="1"; fi
FEATURE="$(cluster_feature_workers "$CORES")"
RESULT_ROOT="${PAPER_EVAL_RESULT_ROOT:-$ROOT/benchmark-results/eurosys/${TAG}-smoke}"

# Smoke output is disposable. Refuse to mix it with an old run unless explicitly reset.
if [[ -e "$RESULT_ROOT" ]]; then
  if [[ "${PAPER_EVAL_SMOKE_RESET:-0}" == 1 ]]; then
    rm -rf "$RESULT_ROOT"
  else
    echo "ERROR: smoke result root already exists: $RESULT_ROOT" >&2
    echo "Use PAPER_EVAL_SMOKE_RESET=1 to replace it, or choose PAPER_EVAL_RESULT_ROOT." >&2
    exit 2
  fi
fi
mkdir -p "$RESULT_ROOT"

export PAPER_EVAL_MACHINE_TAG="$TAG"
export PAPER_EVAL_PROFILE=smoke
export PAPER_EVAL_RESULT_ROOT="$RESULT_ROOT"
export PAPER_EVAL_WORKERS="$WORKERS"
export PAPER_EVAL_FEATURE_WORKERS="$FEATURE"
export PAPER_EVAL_SAMPLES=1
export PAPER_EVAL_GRID_SAMPLES=1
export PAPER_EVAL_COMPUTE_SAMPLES=1
export PAPER_EVAL_IAVL_SAMPLES=1
export PAPER_EVAL_REQUIRE_S4=1
export PAPER_EVAL_RESOURCE_ACCOUNTING=1
export PAPER_EVAL_S1_COMPUTE_SCALE=4
export PAPER_EVAL_S4_COMPUTE_SCALE=4
export PAPER_EVAL_CONSENSUS_WINDOW_MS="${PAPER_EVAL_CONSENSUS_WINDOW_MS:-300}"

source "$ROOT/evaluation/lib/common.sh"
python3 "$ROOT/evaluation/eurosys/capture_machine.py" \
  --output "$RESULT_ROOT/machine.json" \
  --tag "$TAG" --profile smoke --workers "$WORKERS" --samples 1
cluster_write_allocation_metadata "$RESULT_ROOT/cluster-allocation.txt"

STAGES=(
  00_validate.sh
  01_s1_headline.sh
  02_s4_headline.sh
  03_s3_breakdown.sh
  05_conflictlab_upper_bound.sh
  12_conflictlab_semantics.sh
)

LOG="$RESULT_ROOT/cluster-smoke.log"
: > "$LOG"
for stage in "${STAGES[@]}"; do
  echo "===== smoke: $stage =====" | tee -a "$LOG"
  bash "$ROOT/evaluation/experiments/$stage" 2>&1 | tee -a "$LOG"
done

python3 "$ROOT/evaluation/eurosys/validate-result-tree.py" "$RESULT_ROOT" | tee -a "$LOG"
printf 'completed=%s\n' "$(date -Is)" > "$RESULT_ROOT/SMOKE_PASS"

echo
echo "PASS: cluster smoke"
echo "results: $RESULT_ROOT"
echo "workers: $WORKERS"
echo "Next: run evaluation/eurosys/run-cluster-paper.sh from the same allocation policy."
