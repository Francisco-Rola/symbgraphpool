#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
DIAG="$OUT/correctness-diagnostics"
VERIFY="$DIAG/resource-granularity-verification/$STAMP"

manifest="$DIAG/manifests/symbolic-granularity-resource-historical-misses.manifest.json"
mkdir -p "$DIAG/manifests"
python3 - "$OUT/records.jsonl" "$OUT/symbolic-granularity/manifest.json" "$manifest" <<'PY'
import json,sys
from pathlib import Path

records_path=Path(sys.argv[1])
manifest_path=Path(sys.argv[2])
out_path=Path(sys.argv[3])

def run_identity(value):
    meta=value.get('metadata', value)
    parameters=meta.get('parameters', {})
    return (
        meta.get('workload'),
        meta.get('mode'),
        meta.get('run_index'),
        meta.get('seed'),
        meta.get('workers'),
        tuple(sorted(parameters.items())),
    )

records=[]
with records_path.open(encoding='utf-8') as f:
    for line in f:
        if not line.strip():
            continue
        r=json.loads(line)
        m=r.get('metadata', {})
        p=m.get('parameters', {}) if isinstance(m.get('parameters', {}), dict) else {}
        misses=int(r.get('feedback', {}).get('candidate_misses', 0) or 0)
        if (
            m.get('experiment_id')=='conflictlab-v1-symbolic-granularity'
            and m.get('mode')=='probability-only'
            and p.get('operation_mix')=='point-mixed'
            and p.get('symbolic_granularity')=='resource'
            and misses > 0
        ):
            records.append(r)

if len(records)!=6:
    raise SystemExit(
        f'expected six historical probability-only / point-mixed / resource miss records, found {len(records)}'
    )

manifest=json.load(manifest_path.open(encoding='utf-8'))
target={run_identity(r) for r in records}
runs=[run for run in manifest.get('runs', []) if run_identity(run) in target]
if len(runs)!=6:
    raise SystemExit(
        f'expected six matching runs in {manifest_path}, found {len(runs)}'
    )
missing=target-{run_identity(run) for run in runs}
if missing:
    raise SystemExit(f'{len(missing)} historical miss identities are absent from {manifest_path}')

filtered=dict(manifest)
filtered['runs']=runs
out_path.write_text(json.dumps(filtered, indent=2)+'\n', encoding='utf-8')
print(out_path)
PY

python3 - "$manifest" <<'PY'
import json,sys
manifest=json.load(open(sys.argv[1], encoding='utf-8'))
runs=manifest.get('runs', [])
if len(runs)!=6:
    raise SystemExit(f'expected the six historical failing runs, found {len(runs)}')
problems=[]
for run in runs:
    p=run.get('parameters', {})
    if run.get('mode')!='probability-only': problems.append(f"mode={run.get('mode')!r}")
    if p.get('operation_mix')!='point-mixed': problems.append(f"operation_mix={p.get('operation_mix')!r}")
    if p.get('symbolic_granularity')!='resource': problems.append(f"symbolic_granularity={p.get('symbolic_granularity')!r}")
if problems:
    raise SystemExit('historical miss set no longer matches the isolated resource-granularity bug:\n  '+'\n  '.join(problems))
print('isolated historical miss set: 6 probability-only / point-mixed / resource runs')
PY

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would rerun exactly these six historical symbolic-granularity misses:"
  echo "  $manifest"
  echo "Artifacts would be written under:"
  echo "  $VERIFY"
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

mkdir -p "$VERIFY"
cp "$manifest" "$VERIFY/manifest.json"
export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

echo '=== rerun six historical resource-granularity misses ==='
set +e
"$ROOT/runtime/target/release/acg-benchmark" \
  "$VERIFY/manifest.json" \
  "$VERIFY/records.jsonl" \
  "$VERIFY/acceptance.json" \
  "$ROOT" \
  2>&1 | tee "$VERIFY/run.log"
benchmark_rc=${PIPESTATUS[0]}
set -e
printf 'benchmark_exit_code=%s\n' "$benchmark_rc" > "$VERIFY/status.txt"

python3 - "$VERIFY/records.jsonl" <<'PY'
import json,sys
path=sys.argv[1]
records=[json.loads(line) for line in open(path, encoding='utf-8') if line.strip()]
if len(records)!=6:
    raise SystemExit(f'FAIL: expected 6 verification records, found {len(records)}')
non_serial=[]
misses=[]
wrong=[]
for r in records:
    m=r.get('metadata', {})
    p=m.get('parameters', {})
    if p.get('symbolic_granularity')!='resource' or p.get('operation_mix')!='point-mixed' or m.get('mode')!='probability-only':
        wrong.append((m.get('run_index'),m.get('mode'),p.get('symbolic_granularity'),p.get('operation_mix')))
    if r.get('correctness', {}).get('serial_equivalent') is not True:
        non_serial.append(m.get('run_index'))
    count=int(r.get('feedback', {}).get('candidate_misses', 0) or 0)
    if count:
        misses.append((m.get('seed'),m.get('run_index'),count))
print(f'verification_records={len(records)}')
print(f'serial_non_equivalent={len(non_serial)}')
print(f'candidate_miss_records={len(misses)}')
print(f'candidate_misses={sum(item[2] for item in misses)}')
print(f'parameter_errors={len(wrong)}')
if non_serial or misses or wrong:
    if non_serial: print('non_serial_runs=', non_serial)
    if misses: print('remaining_candidate_misses=', misses)
    if wrong: print('parameter_errors_detail=', wrong)
    raise SystemExit(1)
print('PASS: the six historical resource-granularity false negatives are gone and remain serial-equivalent')
PY

echo "artifacts: $VERIFY"
