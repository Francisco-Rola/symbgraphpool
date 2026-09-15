#!/usr/bin/env bash
set -euo pipefail

EVAL_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$EVAL_ROOT/.." && pwd)"
RESULT_ROOT="${PAPER_EVAL_RESULT_ROOT:-$REPO_ROOT/benchmark-results/paper-eval}"
PROFILE="${PAPER_EVAL_PROFILE:-debug}"

physical_cores() {
  if command -v lscpu >/dev/null 2>&1; then
    local n
    n="$(lscpu -p=CORE,SOCKET 2>/dev/null | awk -F, '!/^#/ {print $1 "," $2}' | sort -u | wc -l | tr -d ' ')"
    if [[ "$n" =~ ^[0-9]+$ ]] && (( n > 0 )); then echo "$n"; return; fi
  fi
  nproc 2>/dev/null || echo 1
}

publication_workers() {
  local n="$1" v=1 out=""
  while (( v <= n )); do
    out+="${out:+,}${v}"
    v=$((v * 2))
  done
  if [[ ",$out," != *",$n,"* ]]; then out+=",$n"; fi
  echo "$out"
}

max_worker() {
  local list="${1:-$WORKERS}" max=0 v
  IFS=',' read -r -a _workers <<< "$list"
  for v in "${_workers[@]}"; do
    [[ "$v" =~ ^[0-9]+$ ]] || continue
    (( v > max )) && max="$v"
  done
  (( max > 0 )) || max=1
  echo "$max"
}

PHYSICAL_CORES="$(physical_cores)"
case "$PROFILE" in
  smoke)
    DEFAULT_WORKERS="${PAPER_EVAL_SMOKE_WORKERS:-1,2}"
    DEFAULT_SAMPLES=1
    DEFAULT_S1_BLOCKS=100
    DEFAULT_S4_BLOCKS=100
    ;;
  debug)
    if (( PHYSICAL_CORES >= 6 )); then DEFAULT_WORKERS="1,2,4,6";
    elif (( PHYSICAL_CORES >= 4 )); then DEFAULT_WORKERS="1,2,4";
    else DEFAULT_WORKERS="1,${PHYSICAL_CORES}"; fi
    DEFAULT_SAMPLES=1
    DEFAULT_S1_BLOCKS=300
    DEFAULT_S4_BLOCKS=300
    ;;
  paper)
    DEFAULT_WORKERS="$(publication_workers "$PHYSICAL_CORES")"
    DEFAULT_SAMPLES=5
    DEFAULT_S1_BLOCKS=5000
    DEFAULT_S4_BLOCKS=5000
    ;;
  *) echo "PAPER_EVAL_PROFILE must be smoke|debug|paper" >&2; exit 2 ;;
esac

WORKERS="${PAPER_EVAL_WORKERS:-$DEFAULT_WORKERS}"
SAMPLES="${PAPER_EVAL_SAMPLES:-$DEFAULT_SAMPLES}"
CANONICAL_CONSENSUS_WINDOW_MS="${PAPER_EVAL_CONSENSUS_WINDOW_MS:-300}"
CONSENSUS_WINDOWS_MS="${PAPER_EVAL_CONSENSUS_WINDOWS_MS:-$CANONICAL_CONSENSUS_WINDOW_MS}"
FEATURE_WORKERS="${PAPER_EVAL_FEATURE_WORKERS:-$PHYSICAL_CORES}"
if (( FEATURE_WORKERS > 6 )); then FEATURE_WORKERS=6; fi
if (( FEATURE_WORKERS < 1 )); then FEATURE_WORKERS=1; fi
mkdir -p "$RESULT_ROOT"

run_conflictlab_grid() {
  local grid="$1" out="$2"
  mkdir -p "$out"
  "$REPO_ROOT/tools/internal/run-conflictlab-release-matrix.sh" "$grid" "$out"
}

require_file() {
  [[ -s "$1" ]] || { echo "missing required input: $1" >&2; return 2; }
}

require_dir() {
  [[ -d "$1" ]] || { echo "missing required directory: $1" >&2; return 2; }
}
