#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1/$STAMP}"
PROFILE="${2:-full}"
mkdir -p "$OUT"

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

all_campaigns=(
  core-state
  cutoff-divergence
  serial-cutoff
  compaction-reference
  symbolic-granularity
  prediction-fault-recovery
  adaptation-transitions
  execution-semantics
  block-scaling
  policy-pareto
  bucket-sensitivity
  ordering-sensitivity
  vm-lifecycle
  statistical-headlines
  long-run-soak
)
case "$PROFILE" in
  full) campaigns=("${all_campaigns[@]}") ;;
  core) campaigns=(core-state cutoff-divergence execution-semantics statistical-headlines) ;;
  mechanisms) campaigns=(serial-cutoff compaction-reference symbolic-granularity prediction-fault-recovery adaptation-transitions block-scaling policy-pareto bucket-sensitivity ordering-sensitivity vm-lifecycle long-run-soak) ;;
  *) echo "usage: $0 [output-dir] [full|core|mechanisms]" >&2; exit 2 ;;
esac

cat > "$OUT/suite-environment.txt" <<EOF
profile=$PROFILE
started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git_revision=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)
git_status=$(git -C "$ROOT" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')
physical_core_limit=6
vm_instance_lifecycle_policy=retained-reuse-with-nonbinding-gas; fresh-recycle-control-in-vm-lifecycle
note=No core-count or memory-capacity scaling axis is used in ConflictLab 1.0.
EOF

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

expected_dir="$OUT/.expected-manifests"
mkdir -p "$expected_dir"
campaign_failures=()

run_grid() {
  local name="$1"
  local grid="evaluation/conflictlab/v1-$name.grid.json"
  local expected_manifest="$expected_dir/$name.json"
  local campaign_dir="$OUT/$name"
  local cache_reason

  python3 "$ROOT/scripts/internal/generate-manifest-matrix.py" "$ROOT/$grid" "$expected_manifest" >/dev/null
  if cache_reason=$(python3 "$ROOT/scripts/internal/check-conflictlab-v1-campaign-cache.py" "$campaign_dir" "$expected_manifest" 2>&1); then
    echo "=== ConflictLab 1.0: $name ==="
    echo "REUSE: $cache_reason"
    return 0
  fi

  echo "=== ConflictLab 1.0: $name ==="
  echo "RERUN: ${cache_reason:-no reusable campaign cache}"
  # The matrix runner appends records. Remove any incomplete/stale campaign directory before a
  # rerun so a failed earlier attempt cannot duplicate records in the resumed suite.
  rm -rf "$campaign_dir"
  if "$ROOT/scripts/internal/run-conflictlab-release-matrix.sh" "$grid" "$campaign_dir" \
      2>&1 | tee "$OUT/$name-run.log"; then
    return 0
  fi

  campaign_failures+=("$name")
  echo "WARNING: campaign $name failed acceptance/execution; continuing remaining campaigns" >&2
  return 0
}

for name in "${campaigns[@]}"; do
  run_grid "$name"
done

: > "$OUT/records.jsonl"
for name in "${campaigns[@]}"; do
  if [[ -s "$OUT/$name/records.jsonl" ]]; then
    cat "$OUT/$name/records.jsonl" >> "$OUT/records.jsonl"
  else
    campaign_failures+=("$name:missing-records")
  fi
  if [[ -s "$OUT/$name/acceptance.json" ]]; then
    cp "$OUT/$name/acceptance.json" "$OUT/acceptance-$name.json"
  else
    campaign_failures+=("$name:missing-acceptance")
  fi
done

postprocess_failures=()
validator_args=()
if [[ "$PROFILE" != full ]]; then validator_args+=(--allow-partial); fi
if ! python3 "$ROOT/scripts/internal/validate-conflictlab-v1.py" "$OUT/records.jsonl" "${validator_args[@]}" | tee "$OUT/validation.txt"; then
  postprocess_failures+=(validation)
fi
if ! python3 "$ROOT/scripts/internal/aggregate-experiment.py" "$OUT/records.jsonl" --out-dir "$OUT/aggregate"; then
  postprocess_failures+=(aggregate)
fi
if ! python3 "$ROOT/scripts/internal/summarize-conflictlab-v1.py" "$OUT/records.jsonl" \
    --output "$OUT/results-summary.txt" --markdown "$OUT/paper-analysis.md" > "$OUT/summary-run.log"; then
  postprocess_failures+=(summary)
fi

python3 - "$OUT" <<'PY'
import json,sys
from collections import Counter
from pathlib import Path
out=Path(sys.argv[1]); c=Counter()
for line in (out/'records.jsonl').read_text().splitlines():
    if line.strip(): c[json.loads(line)['metadata']['experiment_id']]+=1
(out/'campaign-counts.json').write_text(json.dumps(dict(sorted(c.items())),indent=2)+'\n')
print(f"combined_records={sum(c.values())}")
for k,v in sorted(c.items()): print(f"  {k}: {v}")
PY

if [[ -s "$OUT/results-summary.txt" ]]; then
  cat "$OUT/results-summary.txt"
  echo
fi

if ((${#campaign_failures[@]} > 0 || ${#postprocess_failures[@]} > 0)); then
  echo "FAIL: ConflictLab 1.0 $PROFILE evaluation completed with problems" >&2
  if ((${#campaign_failures[@]} > 0)); then
    printf '  campaign: %s\n' "${campaign_failures[@]}" >&2
  fi
  if ((${#postprocess_failures[@]} > 0)); then
    printf '  postprocess: %s\n' "${postprocess_failures[@]}" >&2
  fi
  echo "Completed independent campaigns were retained and will be reused on the next run." >&2
  if [[ -x "$ROOT/scripts/internal/collect-conflictlab-v1-debug-bundle.sh" ]]; then
    if debug_bundle=$("$ROOT/scripts/internal/collect-conflictlab-v1-debug-bundle.sh" "$OUT" 2>/dev/null); then
      echo "Debug bundle for upload: $debug_bundle" >&2
    else
      echo "Debug bundle collection failed; upload $OUT/validation.txt and $OUT/records.jsonl." >&2
    fi
  fi
  exit 1
fi

echo "PASS: ConflictLab 1.0 $PROFILE evaluation completed"
echo "upload for analysis:"
echo "  $OUT/results-summary.txt"
echo "  $OUT/validation.txt"
echo "  $OUT/records.jsonl"
echo "  $OUT/aggregate/summary-wide.csv"
echo "  $OUT/aggregate/plot-long.csv"
echo "  $OUT/suite-environment.txt"
echo "or create one self-contained debug/analysis bundle:"
echo "  ./scripts/internal/collect-conflictlab-v1-debug-bundle.sh $OUT"
