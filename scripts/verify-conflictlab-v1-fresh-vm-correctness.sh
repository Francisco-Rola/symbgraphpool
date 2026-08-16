#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
DIAG="$OUT/correctness-diagnostics"
TARGET="$DIAG/fresh-vm-verification/$STAMP"
STATE_DIFFS="$TARGET/state-diffs"

if [[ ! -s "$DIAG/rerun-plan.json" ]]; then
  python3 "$ROOT/scripts/diagnose-conflictlab-v1-correctness.py" "$OUT" >/dev/null
fi

mapfile -t manifests < <(python3 - "$DIAG/rerun-plan.json" <<'PY'
import json,sys
for item in json.load(open(sys.argv[1], encoding='utf-8')):
    if item.get('reason') == 'serial-non-equivalent':
        print(item['manifest'])
PY
)

if ((${#manifests[@]} == 0)); then
  echo "No previously non-serial-equivalent manifests found; nothing to verify."
  exit 0
fi

python3 - "${manifests[@]}" <<'PY'
import json,sys
n=0
for path in sys.argv[1:]:
    doc=json.load(open(path, encoding='utf-8'))
    n += len(doc.get('runs', []))
print(f"previously_failing_runs={n}")
for path in sys.argv[1:]: print(f"  {path}")
PY

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would rerun only those exact failing identities with vm_instance_lifecycle=recycle."
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

mkdir -p "$TARGET" "$STATE_DIFFS"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"
export ACG_CORRECTNESS_DIAGNOSTICS_DIR="$STATE_DIFFS"

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

: > "$TARGET/records.jsonl"
run_failures=0
for source_manifest in "${manifests[@]}"; do
  name="$(basename "$source_manifest" .manifest.json)"
  target="$TARGET/$name"
  mkdir -p "$target"
  python3 - "$source_manifest" "$target/manifest.json" <<'PY'
import json,sys
src,dst=sys.argv[1:]
doc=json.load(open(src, encoding='utf-8'))
for run in doc.get('runs', []):
    run.setdefault('parameters', {})['vm_instance_lifecycle']='recycle'
json.dump(doc, open(dst,'w',encoding='utf-8'), indent=2)
open(dst,'a',encoding='utf-8').write('\n')
PY
  echo "=== fresh-VM verification: $name ==="
  set +e
  "$ROOT/runtime/target/release/acg-benchmark" \
    "$target/manifest.json" \
    "$target/records.jsonl" \
    "$target/acceptance.json" \
    "$ROOT" \
    2>&1 | tee "$target/run.log"
  rc=${PIPESTATUS[0]}
  set -e
  echo "verification_exit_code=$rc" > "$target/status.txt"
  (( rc == 0 )) || run_failures=$((run_failures + 1))
  [[ -s "$target/records.jsonl" ]] && cat "$target/records.jsonl" >> "$TARGET/records.jsonl"
done

set +e
python3 - "$TARGET/records.jsonl" <<'PY'
import json,sys
path=sys.argv[1]
records=[json.loads(line) for line in open(path, encoding='utf-8') if line.strip()]
bad=[r for r in records if r.get('correctness',{}).get('serial_equivalent') is not True]
unsafe=[r for r in records if r.get('metadata',{}).get('parameters',{}).get('vm_instance_lifecycle') != 'recycle']
print(f"verification_records={len(records)}")
print(f"serial_non_equivalent={len(bad)}")
print(f"non_recycle_records={len(unsafe)}")
if bad:
    from collections import Counter
    c=Counter(r.get('metadata',{}).get('experiment_id') for r in bad)
    for k,v in sorted(c.items()): print(f"  bad {k}: {v}")
if bad or unsafe:
    raise SystemExit(1)
PY
verification_rc=$?
set -e

if (( run_failures > 0 || verification_rc != 0 )); then
  echo "FAIL: fresh-VM verification still has execution/acceptance/correctness failures" >&2
  echo "artifacts: $TARGET" >&2
  exit 1
fi

echo "PASS: every previously non-serial-equivalent run is serial-equivalent with fresh/recycled VM instances"
echo "artifacts: $TARGET"
