#!/usr/bin/env bash
# Resume-friendly EuroSys overnight campaign for the local 6-core machine.
# Assumes the S1 headline experiment (01_s1_headline.sh) has already completed
# in the same PAPER_EVAL_RESULT_ROOT. Runs every remaining publication experiment
# with one sample, logs each stage independently, and continues after failures so
# one broken experiment does not waste the rest of the night.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)"
# When copied into the repository (recommended location: evaluation/eurosys/),
# resolve the repository root from git. Otherwise use the current working tree.
if git -C "$PWD" rev-parse --show-toplevel >/dev/null 2>&1; then
  ROOT="$(git -C "$PWD" rev-parse --show-toplevel)"
elif git -C "$ROOT" rev-parse --show-toplevel >/dev/null 2>&1; then
  ROOT="$(git -C "$ROOT" rev-parse --show-toplevel)"
else
  echo "ERROR: run this script from inside the ReSpec repository." >&2
  exit 2
fi
cd "$ROOT"

TAG="${PAPER_EVAL_MACHINE_TAG:-local-6c-paper}"
PROFILE="${PAPER_EVAL_PROFILE:-paper}"
RESULT_ROOT="${PAPER_EVAL_RESULT_ROOT:-$ROOT/benchmark-results/eurosys/$TAG}"

# Force the local publication sweep to the six-core topology and a single sample.
export PAPER_EVAL_MACHINE_TAG="$TAG"
export PAPER_EVAL_PROFILE="$PROFILE"
export PAPER_EVAL_RESULT_ROOT="$RESULT_ROOT"
export PAPER_EVAL_WORKERS="${PAPER_EVAL_WORKERS:-1,2,4,6}"
export PAPER_EVAL_FEATURE_WORKERS="${PAPER_EVAL_FEATURE_WORKERS:-6}"
export PAPER_EVAL_SAMPLES="${PAPER_EVAL_SAMPLES:-1}"
export PAPER_EVAL_COMPUTE_SAMPLES="${PAPER_EVAL_COMPUTE_SAMPLES:-1}"
export PAPER_EVAL_IAVL_SAMPLES="${PAPER_EVAL_IAVL_SAMPLES:-1}"
export PAPER_EVAL_REQUIRE_S4="${PAPER_EVAL_REQUIRE_S4:-1}"
export PAPER_EVAL_RESOURCE_ACCOUNTING="${PAPER_EVAL_RESOURCE_ACCOUNTING:-1}"

S1_RECORDS="$RESULT_ROOT/01-s1/records.jsonl"
if [[ ! -s "$S1_RECORDS" ]]; then
  echo "ERROR: completed S1 headline records not found:" >&2
  echo "  $S1_RECORDS" >&2
  echo "Set PAPER_EVAL_RESULT_ROOT/PAPER_EVAL_MACHINE_TAG to the result tree containing the finished S1 run." >&2
  exit 2
fi

STATE="$RESULT_ROOT/.overnight-rest"
LOG_DIR="$RESULT_ROOT/logs-overnight-rest"
mkdir -p "$STATE" "$LOG_DIR"

# Capture/update machine provenance using exactly the settings for this campaign.
source "$ROOT/evaluation/lib/common.sh"
python3 "$ROOT/evaluation/eurosys/capture_machine.py" \
  --output "$RESULT_ROOT/machine.json" \
  --tag "$TAG" \
  --profile "$PROFILE" \
  --workers "$WORKERS" \
  --samples "$SAMPLES"

cat <<INFO
============================================================
ReSpec EuroSys overnight campaign (S1 headline already done)
repo:        $ROOT
result root: $RESULT_ROOT
machine tag: $TAG
profile:     $PROFILE
workers:     $WORKERS
samples:     $SAMPLES
feature w:   $FEATURE_WORKERS
start:       $(date -Is)
============================================================
INFO

# These are all remaining entries from evaluation/eurosys/run.sh.
# 14 reuses the completed S1 records; 15 and 17 intentionally run shorter
# S1-derived sensitivity experiments and are NOT repetitions of the S1 headline.
EXPERIMENTS=(
  00_validate.sh
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

failures=()
completed=()
skipped=()

run_one() {
  local script="$1"
  local stem="${script%.sh}"
  local marker="$STATE/$stem.done"
  local failed_marker="$STATE/$stem.failed"
  local log="$LOG_DIR/$stem.log"

  if [[ -f "$marker" && "${PAPER_EVAL_FORCE_RERUN:-0}" != 1 ]]; then
    echo
    echo "===== SKIP (already completed): $script ====="
    skipped+=("$script")
    return 0
  fi

  rm -f "$failed_marker"
  echo
  echo "============================================================"
  echo "START $script  $(date -Is)"
  echo "log: $log"
  echo "============================================================"

  # Keep stdout visible for tmux/terminal users while also retaining a per-stage log.
  # PIPESTATUS[0] is the experiment's status rather than tee's status.
  bash "$ROOT/evaluation/experiments/$script" 2>&1 | tee "$log"
  local rc=${PIPESTATUS[0]}

  if (( rc == 0 )); then
    printf 'completed=%s\n' "$(date -Is)" > "$marker"
    completed+=("$script")
    echo "PASS $script  $(date -Is)"
  else
    printf 'failed=%s\nexit_code=%d\nlog=%s\n' "$(date -Is)" "$rc" "$log" > "$failed_marker"
    failures+=("$script:$rc")
    echo "FAIL $script (exit $rc) -- continuing overnight" >&2
  fi
}

for script in "${EXPERIMENTS[@]}"; do
  run_one "$script"
done

echo
if (( ${#failures[@]} == 0 )); then
  echo "===== All remaining experiments passed; generating publication bundle ====="
  POST_LOG="$LOG_DIR/postprocess.log"
  bash "$ROOT/evaluation/eurosys/postprocess.sh" 2>&1 | tee "$POST_LOG"
  post_rc=${PIPESTATUS[0]}
  if (( post_rc == 0 )); then
    printf 'completed=%s\n' "$(date -Is)" > "$STATE/postprocess.done"
  else
    failures+=("postprocess:$post_rc")
    printf 'failed=%s\nexit_code=%d\nlog=%s\n' "$(date -Is)" "$post_rc" "$POST_LOG" > "$STATE/postprocess.failed"
  fi
else
  echo "Skipping final postprocess because one or more experiments failed."
  echo "Fix/re-run only the failed stages, then rerun this script; completed stages will be skipped."
fi

SUMMARY="$STATE/summary.txt"
{
  echo "ReSpec EuroSys overnight summary"
  echo "finished: $(date -Is)"
  echo "result_root: $RESULT_ROOT"
  echo "workers: $WORKERS"
  echo "samples: $SAMPLES"
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

echo
echo "Logs:    $LOG_DIR"
echo "Summary: $SUMMARY"
echo "Paper:   $RESULT_ROOT/paper"

if (( ${#failures[@]} > 0 )); then
  exit 1
fi
