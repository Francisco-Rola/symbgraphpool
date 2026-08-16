#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
DIAG="$OUT/correctness-diagnostics"
RERUN="$DIAG/reruns/$STAMP"
STATE_DIFFS="$DIAG/state-diffs/$STAMP"

python3 "$ROOT/scripts/diagnose-conflictlab-v1-correctness.py" "$OUT" >/dev/null
python3 - "$DIAG/rerun-plan.json" <<'PYEND' > "$DIAG/rerun-manifests.list"
import json,sys
for item in json.load(open(sys.argv[1], encoding='utf-8')):
    if item.get('reason') == 'serial-non-equivalent':
        print(item['manifest'])
PYEND

if [[ ! -s "$DIAG/rerun-manifests.list" ]]; then
  echo "No non-serial-equivalent records found; nothing to reproduce."
  exit 0
fi

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would rerun these exact non-serial-equivalent manifests:"
  sed 's/^/  /' "$DIAG/rerun-manifests.list"
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

mkdir -p "$RERUN" "$STATE_DIFFS"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"
export ACG_CORRECTNESS_DIAGNOSTICS_DIR="$STATE_DIFFS"

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness with correctness diagnostics ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

while IFS= read -r manifest; do
  [[ -n "$manifest" ]] || continue
  name="$(basename "$manifest" .manifest.json)"
  target="$RERUN/$name"
  mkdir -p "$target"
  cp "$manifest" "$target/manifest.json"
  echo "=== diagnostic rerun: $name ==="
  set +e
  "$ROOT/runtime/target/release/acg-benchmark" \
    "$target/manifest.json" \
    "$target/records.jsonl" \
    "$target/acceptance.json" \
    "$ROOT" \
    2>&1 | tee "$target/run.log"
  rc=${PIPESTATUS[0]}
  set -e
  echo "diagnostic_exit_code=$rc" > "$target/diagnostic-status.txt"
  if [[ -s "$target/records.jsonl" ]]; then
    python3 "$ROOT/scripts/aggregate-experiment.py" "$target/records.jsonl" --out-dir "$target/aggregate" || true
  fi
done < "$DIAG/rerun-manifests.list"

echo
echo "Diagnostic reruns complete."
python3 "$ROOT/scripts/summarize-conflictlab-v1-state-diffs.py" "$STATE_DIFFS" --output-dir "$DIAG" || true
printf '%s\n' "$STATE_DIFFS" > "$DIAG/latest-state-diff-dir.txt"
printf '%s\n' "$RERUN" > "$DIAG/latest-rerun-dir.txt"
echo "State mismatches (one JSON file per reproduced bad run): $STATE_DIFFS"
echo "Rerun records and logs: $RERUN"
echo "Send me $DIAG/summary.txt, $DIAG/state-mismatch-summary.txt, and $DIAG/state-mismatch-details.csv."
