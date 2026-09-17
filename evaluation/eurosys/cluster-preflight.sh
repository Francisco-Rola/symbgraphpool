#!/usr/bin/env bash
# Validate a cluster allocation and all frozen inputs before an expensive run.

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
source "$ROOT/evaluation/eurosys/cluster-common.sh"

failures=0
warnings=0

fail() { echo "ERROR: $*" >&2; failures=$((failures + 1)); }
warn() { echo "WARN:  $*" >&2; warnings=$((warnings + 1)); }
pass() { echo "PASS:  $*"; }

printf '%s\n' "============================================================"
printf '%s\n' "EuroSys cluster preflight"
printf 'repo:                 %s\n' "$ROOT"
printf 'host:                 %s\n' "$(hostname -f 2>/dev/null || hostname)"
printf 'affinity logical:     %s\n' "$(cluster_affinity_logical_cpus)"
printf 'affinity physical:    %s\n' "$(cluster_affinity_physical_cores)"
printf 'publication workers:  %s\n' "$(cluster_publication_workers "$(cluster_affinity_physical_cores)")"
printf 'feature workers:      %s\n' "$(cluster_feature_workers "$(cluster_affinity_physical_cores)")"
printf '%s\n' "============================================================"

for cmd in git python3 go cargo rustc lscpu sha256sum; do
  if command -v "$cmd" >/dev/null 2>&1; then
    pass "command available: $cmd"
  else
    fail "missing command: $cmd"
  fi
done
if [[ -x /usr/bin/time ]]; then pass "/usr/bin/time available"; else fail "missing /usr/bin/time"; fi

if ! cluster_require_clean_tracked_tree; then failures=$((failures + 1)); else pass "tracked Git tree is clean"; fi
if git diff --check >/dev/null; then pass "git diff --check"; else fail "git diff --check failed"; fi

if grep -qiE 'microsoft|wsl' /proc/version /proc/sys/kernel/osrelease 2>/dev/null; then
  warn "host appears to be WSL; use bare-metal Linux for the publication campaign"
else
  pass "host is not detected as WSL"
fi

if command -v rustup >/dev/null 2>&1; then
  if rustup target list --installed | grep -qx 'wasm32-unknown-unknown'; then
    pass "Rust wasm32-unknown-unknown target installed"
  else
    fail "Rust target wasm32-unknown-unknown is not installed"
  fi
else
  warn "rustup not found; cannot verify wasm32-unknown-unknown target"
fi

require_input() {
  local p="$1"
  if [[ -s "$p" ]]; then pass "input: ${p#$ROOT/}"; else fail "missing/empty input: $p"; fi
}
require_directory() {
  local p="$1"
  if [[ -d "$p" ]]; then pass "directory: ${p#$ROOT/}"; else fail "missing directory: $p"; fi
}

for dataset in s1 s3 s4; do
  exec="$ROOT/benchmarks/corpora/vegeta-ethereum/$dataset/native-execution"
  require_input "$exec/execution-manifest.json"
  require_input "$exec/execution-plan.jsonl"
done
require_directory "$ROOT/benchmarks/symbolic/native-s3"
require_directory "$ROOT/benchmarks/corpora/vegeta-ethereum/s4/native-execution/symbolic"

S4_READY="$ROOT/benchmarks/corpora/vegeta-ethereum/s4/native-plan/readiness.txt"
if [[ -s "$S4_READY" ]]; then
  if grep -q 'selected profile ready: PASS' "$S4_READY"; then
    pass "S4 scheduler-fidelity readiness is frozen PASS"
  else
    fail "S4 readiness file does not contain 'selected profile ready: PASS'"
  fi
else
  fail "missing S4 readiness file: $S4_READY"
fi

# Syntax/compile checks are cheap and catch accidental transfer corruption.
while IFS= read -r -d '' script; do
  if ! bash -n "$script"; then fail "shell syntax: ${script#$ROOT/}"; fi
done < <(find evaluation/experiments evaluation/eurosys -type f -name '*.sh' -print0)
if (( failures == 0 )); then pass "shell syntax checks"; fi

if python3 -m py_compile evaluation/eurosys/*.py evaluation/lib/*.py >/dev/null 2>&1; then
  pass "Python compile checks"
else
  fail "Python compile checks failed"
fi

# The publication S4 bundle has historically been the most fragile prepared input.
# Re-run its prepared-only validator unless explicitly disabled.
if [[ "${PAPER_EVAL_CLUSTER_DEEP_PREFLIGHT:-1}" == 1 ]]; then
  validator="$ROOT/tools/vegeta/validate-native-s3-execution.py"
  if [[ -f "$validator" ]]; then
    if python3 "$validator" \
      --output-dir "$ROOT/benchmarks/corpora/vegeta-ethereum/s4/native-execution" \
      --prepared-only >/tmp/eurosys-s4-preflight.$$.log 2>&1; then
      pass "S4 prepared-execution validator"
      rm -f /tmp/eurosys-s4-preflight.$$.log
    else
      cat /tmp/eurosys-s4-preflight.$$.log >&2 || true
      rm -f /tmp/eurosys-s4-preflight.$$.log
      fail "S4 prepared-execution validator failed"
    fi
  else
    fail "missing prepared-execution validator: $validator"
  fi
fi

RESULT_PARENT="${PAPER_EVAL_RESULT_ROOT:-$ROOT/benchmark-results/eurosys}"
mkdir -p "$RESULT_PARENT" 2>/dev/null || true
probe="$RESULT_PARENT"
while [[ ! -d "$probe" && "$probe" != / ]]; do probe="$(dirname "$probe")"; done
free_kb="$(df -Pk "$probe" | awk 'NR==2 {print $4}')"
free_gb=$(( free_kb / 1024 / 1024 ))
min_gb="${PAPER_EVAL_MIN_FREE_GB:-20}"
if (( free_gb < min_gb )); then
  fail "only ${free_gb} GiB free at $probe; require at least ${min_gb} GiB (override PAPER_EVAL_MIN_FREE_GB)"
else
  pass "disk space: ${free_gb} GiB free at $probe"
fi

sockets="$(lscpu -p=SOCKET 2>/dev/null | awk '!/^#/ {print $1}' | sort -u | wc -l | tr -d ' ')"
numa="$(lscpu -p=NODE 2>/dev/null | awk '!/^#/ && $1 != "" {print $1}' | sort -u | wc -l | tr -d ' ')"
if [[ "$sockets" =~ ^[0-9]+$ ]] && (( sockets > 1 )); then
  warn "host has $sockets sockets; keep CPU/NUMA placement fixed across all strategies"
fi
if [[ "$numa" =~ ^[0-9]+$ ]] && (( numa > 1 )); then
  warn "host has $numa NUMA nodes; record the allocation/binding used by the batch scheduler"
fi

printf '\npreflight: failures=%d warnings=%d\n' "$failures" "$warnings"
if (( failures > 0 )); then exit 1; fi
printf 'PASS: cluster preflight complete\n'
