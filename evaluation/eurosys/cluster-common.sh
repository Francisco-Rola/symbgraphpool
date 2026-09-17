#!/usr/bin/env bash
# Shared helpers for running the EuroSys campaign on an allocated cluster node.
# This file is sourced by the cluster preflight/smoke/paper runners.

set -euo pipefail

EUROSYS_CLUSTER_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

cluster_affinity_logical_cpus() {
  python3 - <<'PY'
import os
try:
    cpus = os.sched_getaffinity(0)
    print(max(1, len(cpus)))
except (AttributeError, OSError):
    print(max(1, os.cpu_count() or 1))
PY
}

cluster_affinity_physical_cores() {
  python3 - <<'PY'
import os
from pathlib import Path

try:
    cpus = sorted(os.sched_getaffinity(0))
except (AttributeError, OSError):
    cpus = list(range(os.cpu_count() or 1))

cores = set()
for cpu in cpus:
    base = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology")
    try:
        core = (base / "core_id").read_text().strip()
        package = (base / "physical_package_id").read_text().strip()
        cores.add((package, core))
    except OSError:
        cores.add(("cpu", str(cpu)))

print(max(1, len(cores)))
PY
}

cluster_affinity_cpu_list() {
  python3 - <<'PY'
import os
try:
    cpus = sorted(os.sched_getaffinity(0))
except (AttributeError, OSError):
    cpus = list(range(os.cpu_count() or 1))
print(",".join(map(str, cpus)))
PY
}

cluster_publication_workers() {
  local n="$1" v=1 out=""
  while (( v <= n )); do
    out+="${out:+,}${v}"
    v=$((v * 2))
  done
  if [[ ",$out," != *",$n,"* ]]; then
    out+=",$n"
  fi
  printf '%s\n' "$out"
}

cluster_feature_workers() {
  local n="$1"
  if [[ -n "${PAPER_EVAL_FEATURE_WORKERS:-}" ]]; then
    printf '%s\n' "$PAPER_EVAL_FEATURE_WORKERS"
  elif (( n > 6 )); then
    printf '6\n'
  else
    printf '%s\n' "$n"
  fi
}

cluster_default_tag() {
  local n="$1"
  printf '%s-%sc\n' "$(hostname -s)" "$n"
}

cluster_require_clean_tracked_tree() {
  if [[ "${PAPER_EVAL_ALLOW_DIRTY:-0}" == 1 ]]; then
    return 0
  fi
  local dirty
  dirty="$(git -C "$EUROSYS_CLUSTER_ROOT" status --porcelain --untracked-files=no)"
  if [[ -n "$dirty" ]]; then
    echo "ERROR: tracked files are modified. Commit/freeze the evaluation revision first:" >&2
    printf '%s\n' "$dirty" >&2
    echo "Set PAPER_EVAL_ALLOW_DIRTY=1 only for a non-publication diagnostic run." >&2
    return 2
  fi
}

cluster_write_allocation_metadata() {
  local out="$1"
  mkdir -p "$(dirname "$out")"
  {
    echo "captured=$(date -Is)"
    echo "hostname=$(hostname -f 2>/dev/null || hostname)"
    echo "affinity_logical_cpus=$(cluster_affinity_logical_cpus)"
    echo "affinity_physical_cores=$(cluster_affinity_physical_cores)"
    echo "affinity_cpu_list=$(cluster_affinity_cpu_list)"
    echo "SLURM_JOB_ID=${SLURM_JOB_ID:-}"
    echo "SLURM_JOB_NAME=${SLURM_JOB_NAME:-}"
    echo "SLURM_CPUS_ON_NODE=${SLURM_CPUS_ON_NODE:-}"
    echo "SLURM_CPUS_PER_TASK=${SLURM_CPUS_PER_TASK:-}"
    echo "SLURM_JOB_CPUS_PER_NODE=${SLURM_JOB_CPUS_PER_NODE:-}"
    echo "SLURM_MEM_PER_NODE=${SLURM_MEM_PER_NODE:-}"
    echo "SLURM_MEM_PER_CPU=${SLURM_MEM_PER_CPU:-}"
    echo "SLURM_NODELIST=${SLURM_NODELIST:-}"
    echo "PBS_JOBID=${PBS_JOBID:-}"
    echo "LSB_JOBID=${LSB_JOBID:-}"
    echo "git_commit=$(git -C "$EUROSYS_CLUSTER_ROOT" rev-parse HEAD 2>/dev/null || true)"
    echo "git_describe=$(git -C "$EUROSYS_CLUSTER_ROOT" describe --always --dirty 2>/dev/null || true)"
    echo
    echo "[lscpu]"
    lscpu 2>/dev/null || true
    echo
    echo "[numactl]"
    numactl --hardware 2>/dev/null || true
  } > "$out"
}
