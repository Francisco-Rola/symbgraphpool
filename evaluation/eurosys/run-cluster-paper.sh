#!/usr/bin/env bash
# Resume-safe EuroSys publication campaign for an allocated cluster node.
# Defaults to three independent samples per ordinary experiment and three seeds
# per ConflictLab paper grid. Completed stages are marked and skipped on rerun.

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/evaluation/eurosys/cluster-common.sh"

"$ROOT/evaluation/eurosys/cluster-preflight.sh"

CORES="$(cluster_affinity_physical_cores)"
TAG="${PAPER_EVAL_MACHINE_TAG:-$(cluster_default_tag "$CORES")}"
WORKERS="${PAPER_EVAL_WORKERS:-$(cluster_publication_workers "$CORES")}"
FEATURE="$(cluster_feature_workers "$CORES")"
SAMPLES="${PAPER_EVAL_SAMPLES:-3}"
GRID_SAMPLES="${PAPER_EVAL_GRID_SAMPLES:-$SAMPLES}"
RESULT_ROOT="${PAPER_EVAL_RESULT_ROOT:-$ROOT/benchmark-results/eurosys/${TAG}-paper-${SAMPLES}s}"

if ! [[ "$SAMPLES" =~ ^[1-9][0-9]*$ ]]; then echo "invalid PAPER_EVAL_SAMPLES=$SAMPLES" >&2; exit 2; fi
if ! [[ "$GRID_SAMPLES" =~ ^[1-9][0-9]*$ ]]; then echo "invalid PAPER_EVAL_GRID_SAMPLES=$GRID_SAMPLES" >&2; exit 2; fi

export PAPER_EVAL_MACHINE_TAG="$TAG"
export PAPER_EVAL_PROFILE=paper
export PAPER_EVAL_RESULT_ROOT="$RESULT_ROOT"
export PAPER_EVAL_WORKERS="$WORKERS"
export PAPER_EVAL_FEATURE_WORKERS="$FEATURE"
export PAPER_EVAL_SAMPLES="$SAMPLES"
export PAPER_EVAL_GRID_SAMPLES="$GRID_SAMPLES"
export PAPER_EVAL_COMPUTE_SAMPLES="${PAPER_EVAL_COMPUTE_SAMPLES:-$SAMPLES}"
export PAPER_EVAL_IAVL_SAMPLES="${PAPER_EVAL_IAVL_SAMPLES:-$SAMPLES}"
export PAPER_EVAL_REQUIRE_S4=1
export PAPER_EVAL_RESOURCE_ACCOUNTING=1
export PAPER_EVAL_S1_COMPUTE_SCALE="${PAPER_EVAL_S1_COMPUTE_SCALE:-4}"
export PAPER_EVAL_S4_COMPUTE_SCALE="${PAPER_EVAL_S4_COMPUTE_SCALE:-4}"
export PAPER_EVAL_CONSENSUS_WINDOW_MS="${PAPER_EVAL_CONSENSUS_WINDOW_MS:-300}"

mkdir -p "$RESULT_ROOT"
STATE="$RESULT_ROOT/.cluster-paper"
LOG_DIR="$RESULT_ROOT/logs-cluster-paper"
mkdir -p "$STATE" "$LOG_DIR"

# Never append a resumed campaign to measurements from another revision/configuration.
CURRENT_COMMIT="$(git rev-parse HEAD)"
CONFIG="$STATE/config.env"
if [[ -s "$CONFIG" ]]; then
  # shellcheck disable=SC1090
  source "$CONFIG"
  [[ "$FROZEN_GIT_COMMIT" == "$CURRENT_COMMIT" ]] || {
    echo "ERROR: existing result root belongs to git $FROZEN_GIT_COMMIT, current $CURRENT_COMMIT" >&2; exit 2; }
  [[ "$FROZEN_WORKERS" == "$WORKERS" ]] || {
    echo "ERROR: existing result root workers=$FROZEN_WORKERS, requested $WORKERS" >&2; exit 2; }
  [[ "$FROZEN_SAMPLES" == "$SAMPLES" ]] || {
    echo "ERROR: existing result root samples=$FROZEN_SAMPLES, requested $SAMPLES" >&2; exit 2; }
  [[ "$FROZEN_GRID_SAMPLES" == "$GRID_SAMPLES" ]] || {
    echo "ERROR: existing result root grid samples=$FROZEN_GRID_SAMPLES, requested $GRID_SAMPLES" >&2; exit 2; }
else
  cat > "$CONFIG" <<CFG
FROZEN_GIT_COMMIT='$CURRENT_COMMIT'
FROZEN_WORKERS='$WORKERS'
FROZEN_SAMPLES='$SAMPLES'
FROZEN_GRID_SAMPLES='$GRID_SAMPLES'
FROZEN_FEATURE_WORKERS='$FEATURE'
FROZEN_CONSENSUS_WINDOW_MS='$PAPER_EVAL_CONSENSUS_WINDOW_MS'
FROZEN_S1_COMPUTE_SCALE='$PAPER_EVAL_S1_COMPUTE_SCALE'
FROZEN_S4_COMPUTE_SCALE='$PAPER_EVAL_S4_COMPUTE_SCALE'
CFG
fi

source "$ROOT/evaluation/lib/common.sh"
python3 "$ROOT/evaluation/eurosys/capture_machine.py" \
  --output "$RESULT_ROOT/machine.json" \
  --tag "$TAG" --profile paper --workers "$WORKERS" --samples "$SAMPLES"
cluster_write_allocation_metadata "$RESULT_ROOT/cluster-allocation.txt"

cat <<INFO
============================================================
EuroSys cluster publication campaign
repo:             $ROOT
commit:           $CURRENT_COMMIT
result root:      $RESULT_ROOT
machine tag:      $TAG
allocated cores:  $CORES physical (affinity-aware)
workers:          $WORKERS
feature workers:  $FEATURE
samples:          $SAMPLES
grid seeds:       $GRID_SAMPLES
compute samples:  $PAPER_EVAL_COMPUTE_SAMPLES
IAVL samples:     $PAPER_EVAL_IAVL_SAMPLES
consensus window: $PAPER_EVAL_CONSENSUS_WINDOW_MS ms
start:            $(date -Is)
============================================================
INFO

EXPERIMENTS=(
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
  17_iavl_sensitivity.sh
)

completed=()
skipped=()
failures=()

run_one() {
  local script="$1" stem="${1%.sh}"
  local done="$STATE/$stem.done" failed="$STATE/$stem.failed"
  local log="$LOG_DIR/$stem.log"

  if [[ -s "$done" && "${PAPER_EVAL_FORCE_RERUN:-0}" != 1 ]]; then
    echo "===== SKIP complete: $script ====="
    skipped+=("$script")
    return 0
  fi

  rm -f "$failed"
  echo
  echo "============================================================"
  echo "START $script  $(date -Is)"
  echo "log: $log"
  echo "============================================================"

  set +e
  bash "$ROOT/evaluation/experiments/$script" 2>&1 | tee "$log"
  rc=${PIPESTATUS[0]}
  set -e

  if (( rc == 0 )); then
    printf 'completed=%s\n' "$(date -Is)" > "$done"
    completed+=("$script")
    echo "PASS $script  $(date -Is)"
    return 0
  fi

  printf 'failed=%s\nexit_code=%d\nlog=%s\n' "$(date -Is)" "$rc" "$log" > "$failed"
  failures+=("$script:$rc")
  echo "FAIL $script (exit $rc)" >&2
  if [[ "${PAPER_EVAL_CONTINUE_ON_FAILURE:-0}" != 1 ]]; then
    return "$rc"
  fi
  return 0
}

campaign_rc=0
for script in "${EXPERIMENTS[@]}"; do
  if ! run_one "$script"; then
    campaign_rc=1
    break
  fi
done

if (( campaign_rc == 0 && ${#failures[@]} == 0 )); then
  echo
  echo "===== Postprocessing publication artifacts ====="
  POST_LOG="$LOG_DIR/postprocess.log"
  set +e
  bash "$ROOT/evaluation/eurosys/postprocess.sh" 2>&1 | tee "$POST_LOG"
  post_rc=${PIPESTATUS[0]}
  set -e
  if (( post_rc == 0 )); then
    printf 'completed=%s\n' "$(date -Is)" > "$STATE/postprocess.done"
  else
    failures+=("postprocess:$post_rc")
    campaign_rc=1
    printf 'failed=%s\nexit_code=%d\nlog=%s\n' "$(date -Is)" "$post_rc" "$POST_LOG" > "$STATE/postprocess.failed"
  fi
fi

if (( campaign_rc == 0 && ${#failures[@]} == 0 )); then
  echo
  echo "===== Integrity scan ====="
  if python3 "$ROOT/evaluation/eurosys/validate-result-tree.py" "$RESULT_ROOT" | tee "$LOG_DIR/integrity.log"; then
    printf 'completed=%s\n' "$(date -Is)" > "$STATE/integrity.done"
  else
    failures+=("integrity:1")
    campaign_rc=1
  fi
fi

SUMMARY="$STATE/summary.txt"
{
  echo "EuroSys cluster publication summary"
  echo "finished: $(date -Is)"
  echo "git: $CURRENT_COMMIT"
  echo "result_root: $RESULT_ROOT"
  echo "workers: $WORKERS"
  echo "samples: $SAMPLES"
  echo "grid_samples: $GRID_SAMPLES"
  echo
  echo "completed (${#completed[@]}):"
  printf '  %s\n' "${completed[@]:-}"
  echo
  echo "already complete/skipped (${#skipped[@]}):"
  printf '  %s\n' "${skipped[@]:-}"
  echo
  echo "failures (${#failures[@]}):"
  printf '  %s\n' "${failures[@]:-}"
} | tee "$SUMMARY"

if (( campaign_rc != 0 || ${#failures[@]} > 0 )); then
  echo >&2
  echo "Campaign incomplete. Fix the failed stage and rerun this same command; completed stages will be skipped." >&2
  exit 1
fi

echo
echo "PASS: complete three-sample cluster campaign"
echo "Paper artifacts: $RESULT_ROOT/paper"
echo "Review bundle command:"
echo "  PAPER_EVAL_RESULT_ROOT=\"$RESULT_ROOT\" evaluation/eurosys/make-review-bundle.sh eurosys-cluster-review.zip"
