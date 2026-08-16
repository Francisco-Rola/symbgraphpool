#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
DIAG="$OUT/correctness-diagnostics"
TARGET="$DIAG/retained-vm-verification/$STAMP"
MAX_GAS="18446744073709551615"

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
  echo "Would rerun the exact failing identities twice:"
  echo "  retained: vm_instance_lifecycle=reuse, vm_gas_limit=$MAX_GAS"
  echo "  fresh:    vm_instance_lifecycle=recycle, vm_gas_limit=$MAX_GAS"
  echo "Then compare canonical state digests pairwise."
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

mkdir -p "$TARGET/retained" "$TARGET/fresh"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

: > "$TARGET/retained/records.jsonl"
: > "$TARGET/fresh/records.jsonl"
run_failures=0

for lifecycle in retained fresh; do
  if [[ "$lifecycle" == retained ]]; then
    vm_mode=reuse
  else
    vm_mode=recycle
  fi
  for source_manifest in "${manifests[@]}"; do
    name="$(basename "$source_manifest" .manifest.json)"
    target="$TARGET/$lifecycle/$name"
    mkdir -p "$target"
    python3 - "$source_manifest" "$target/manifest.json" "$vm_mode" "$MAX_GAS" <<'PY'
import json,sys
src,dst,lifecycle,gas=sys.argv[1:]
doc=json.load(open(src, encoding='utf-8'))
for run in doc.get('runs', []):
    params=run.setdefault('parameters', {})
    params['vm_instance_lifecycle']=lifecycle
    params['vm_gas_limit']=gas
json.dump(doc, open(dst,'w',encoding='utf-8'), indent=2)
open(dst,'a',encoding='utf-8').write('\n')
PY
    echo "=== retained-VM semantic verification: $lifecycle/$name ==="
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
    [[ -s "$target/records.jsonl" ]] && cat "$target/records.jsonl" >> "$TARGET/$lifecycle/records.jsonl"
  done
done

set +e
python3 - "$TARGET/retained/records.jsonl" "$TARGET/fresh/records.jsonl" "$MAX_GAS" <<'PY'
import json,sys
from collections import Counter
retained_path,fresh_path,max_gas=sys.argv[1:]

def load(path):
    return [json.loads(line) for line in open(path, encoding='utf-8') if line.strip()]

def key(r):
    md=r.get('metadata',{})
    params=dict(md.get('parameters',{}))
    params.pop('vm_instance_lifecycle',None)
    return (md.get('experiment_id'),md.get('mode'),md.get('run_index'),md.get('seed'),md.get('workers'),tuple(sorted(params.items())))

retained=load(retained_path)
fresh=load(fresh_path)
retained_bad=[r for r in retained if r.get('correctness',{}).get('serial_equivalent') is not True]
fresh_bad=[r for r in fresh if r.get('correctness',{}).get('serial_equivalent') is not True]
retained_wrong=[r for r in retained if r.get('metadata',{}).get('parameters',{}).get('vm_instance_lifecycle')!='reuse' or r.get('metadata',{}).get('parameters',{}).get('vm_gas_limit')!=max_gas]
fresh_wrong=[r for r in fresh if r.get('metadata',{}).get('parameters',{}).get('vm_instance_lifecycle')!='recycle' or r.get('metadata',{}).get('parameters',{}).get('vm_gas_limit')!=max_gas]
R={key(r):r for r in retained}; F={key(r):r for r in fresh}
missing=sorted(set(R)^set(F))
digest_mismatch=[]
for k in sorted(set(R)&set(F)):
    rd=R[k].get('correctness',{}).get('canonical_state_digest')
    fd=F[k].get('correctness',{}).get('canonical_state_digest')
    if rd!=fd:
        digest_mismatch.append((k,rd,fd))
print(f"retained_records={len(retained)}")
print(f"fresh_records={len(fresh)}")
print(f"retained_serial_non_equivalent={len(retained_bad)}")
print(f"fresh_serial_non_equivalent={len(fresh_bad)}")
print(f"paired_run_identities={len(set(R)&set(F))}")
print(f"retained_fresh_digest_mismatches={len(digest_mismatch)}")
print(f"parameter_or_pairing_errors={len(retained_wrong)+len(fresh_wrong)+len(missing)}")
if digest_mismatch:
    for k,rd,fd in digest_mismatch[:20]:
        print(f"  mismatch {k[:4]} retained={rd} fresh={fd}")
if retained_bad:
    for exp,n in sorted(Counter(r.get('metadata',{}).get('experiment_id') for r in retained_bad).items()):
        print(f"  retained bad {exp}: {n}")
if retained_bad or fresh_bad or retained_wrong or fresh_wrong or missing or digest_mismatch:
    raise SystemExit(1)
PY
verification_rc=$?
set -e

if (( run_failures > 0 || verification_rc != 0 )); then
  echo "FAIL: benchmark-scoped retained reuse is not fresh-state-equivalent for the reproduced failures" >&2
  echo "artifacts: $TARGET" >&2
  exit 1
fi

echo "PASS: retained reuse with non-binding cumulative gas matches fresh/recycle state for every previously failing identity"
echo "artifacts: $TARGET"
