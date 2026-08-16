#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
VERIFY="$OUT/correctness-diagnostics/compaction-normalized-warmup-verification/$STAMP"
SOURCE_RECORDS="$OUT/compaction-reference/records.jsonl"
SOURCE_MANIFEST="$OUT/compaction-reference/manifest.json"
MANIFEST="$VERIFY/manifest.json"

if [[ ! -f "$SOURCE_RECORDS" || ! -f "$SOURCE_MANIFEST" ]]; then
  echo "missing cached compaction-reference records/manifest under $OUT" >&2
  exit 1
fi

mkdir -p "$VERIFY"
python3 - "$SOURCE_RECORDS" "$SOURCE_MANIFEST" "$MANIFEST" <<'PY'
import json,sys
from collections import defaultdict
from pathlib import Path

records_path=Path(sys.argv[1])
manifest_path=Path(sys.argv[2])
out_path=Path(sys.argv[3])
records=[json.loads(line) for line in records_path.open(encoding='utf-8') if line.strip()]
manifest=json.load(manifest_path.open(encoding='utf-8'))

pairs=defaultdict(dict)
for r in records:
    m=r['metadata']
    p=dict(m['parameters'])
    toggle=p.pop('acg.compact_equivalence_groups')
    key=(m['mode'],m['seed'],tuple(sorted(p.items())))
    pairs[key][toggle]=r

meaningful=[
    ('scheduling','candidate_edges'),
    ('scheduling','low_edges'),
    ('scheduling','soft_edges'),
    ('scheduling','hard_edges'),
    ('scheduling','wave_count'),
    ('scheduling','max_wave_width'),
    ('scheduling','scheduled_dependencies'),
    ('feedback','positive_observations'),
    ('feedback','negative_observations'),
    ('feedback','candidate_misses'),
    ('adaptive_state','mean_probability_q16'),
    ('adaptive_state','mean_confidence_q16'),
]

bad_keys=[]
for key,pair in pairs.items():
    if set(pair)!={'true','false'}:
        continue
    c,d=pair['true'],pair['false']
    if any(c.get(section,{}).get(field)!=d.get(section,{}).get(field) for section,field in meaningful):
        bad_keys.append(key)

if not bad_keys:
    raise SystemExit('no historical compact/dense semantic-learning mismatches found to verify')

# Select both run identities for every historically divergent pair. run_index is unique inside the
# campaign manifest and avoids reconstructing its generation order.
selected_indices=set()
for key in bad_keys:
    for record in pairs[key].values():
        selected_indices.add(record['metadata']['run_index'])

runs=[]
for run in manifest.get('runs',[]):
    if run.get('run_index') not in selected_indices:
        continue
    copied=json.loads(json.dumps(run))
    params=copied.setdefault('parameters',{})
    params['acg.warmup_compact_equivalence_groups']='false'
    params['acg.warmup_workers']='1'
    params['consensus_cutoff_ms']='5000'
    runs.append(copied)

if len(runs)!=2*len(bad_keys):
    raise SystemExit(
        f'expected {2*len(bad_keys)} runs for {len(bad_keys)} divergent pairs, found {len(runs)}'
    )

filtered=dict(manifest)
filtered['experiment_id']='conflictlab-v1-compaction-normalized-warmup-verification'
filtered['runs']=runs
out_path.write_text(json.dumps(filtered,indent=2)+'\n',encoding='utf-8')
print(f'historical_divergent_pairs={len(bad_keys)}')
print(f'verification_runs={len(runs)}')
print(out_path)
PY

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would rerun only the historically divergent compact/dense pairs with a dense single-worker warm-up; semantic invariants are gated separately from parallel-path feedback drift."
  echo "Manifest: $MANIFEST"
  echo "Artifacts: $VERIFY"
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

export ACG_BUILD_PROFILE=release
export ACG_CONFLICTLAB_WASM="${ACG_CONFLICTLAB_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm}"

echo '=== build real ConflictLab Wasm ==='
cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab --release --target wasm32-unknown-unknown

echo '=== build release benchmark harness ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --bin acg-benchmark --release

echo '=== verify normalized-warmup compact/dense pairs ==='
"$ROOT/runtime/target/release/acg-benchmark" \
  "$MANIFEST" \
  "$VERIFY/records.jsonl" \
  "$VERIFY/acceptance.json" \
  "$ROOT" \
  2>&1 | tee "$VERIFY/run.log"

python3 - "$VERIFY/records.jsonl" <<'PY'
import json,sys
from collections import defaultdict
records=[json.loads(line) for line in open(sys.argv[1],encoding='utf-8') if line.strip()]
pairs=defaultdict(dict)
for r in records:
    m=r['metadata']; p=dict(m['parameters'])
    toggle=p.pop('acg.compact_equivalence_groups')
    if p.get('acg.warmup_compact_equivalence_groups')!='false':
        raise SystemExit('FAIL: verification run did not use dense normalized warm-up')
    if p.get('acg.warmup_workers')!='1':
        raise SystemExit('FAIL: verification run did not use deterministic single-worker warm-up')
    key=(m['mode'],m['seed'],tuple(sorted(p.items())))
    pairs[key][toggle]=r

semantic_fields=[
    ('scheduling','candidate_edges'),('scheduling','low_edges'),
    ('scheduling','soft_edges'),('scheduling','hard_edges'),
    ('feedback','candidate_misses'),
]
semantic_problems=[]
strict_reductions=0
feedback_drift_pairs=set(); posterior_drift_pairs=set(); schedule_drift_pairs=set()
max_drift={
    'positive_observations':0,
    'negative_observations':0,
    'mean_probability_q16':0,
    'mean_confidence_q16':0,
    'wave_count':0,
    'max_wave_width':0,
    'scheduled_dependencies':0,
}
for key,pair in pairs.items():
    if set(pair)!={'true','false'}:
        semantic_problems.append((key,'incomplete_pair',sorted(pair)))
        continue
    c,d=pair['true'],pair['false']
    for label,r in [('compact',c),('dense',d)]:
        consensus=r.get('consensus',{})
        if consensus.get('cutoff_reached'):
            semantic_problems.append((key,f'{label}.cutoff_reached',True))
        if consensus.get('prepared_receipts')!=consensus.get('candidate_transactions'):
            semantic_problems.append((key,f'{label}.prepared_receipts',(
                consensus.get('prepared_receipts'),consensus.get('candidate_transactions'))))
        if r.get('correctness',{}).get('serial_equivalent') is not True:
            semantic_problems.append((key,f'{label}.serial_equivalent',False))
    for section,field in semantic_fields:
        cv=c.get(section,{}).get(field); dv=d.get(section,{}).get(field)
        if cv!=dv:
            semantic_problems.append((key,f'{section}.{field}',(cv,dv)))
    if c.get('correctness',{}).get('canonical_state_digest')!=d.get('correctness',{}).get('canonical_state_digest'):
        semantic_problems.append((key,'correctness.canonical_state_digest','mismatch'))
    cm=c.get('scheduling',{}).get('materialized_candidate_edges',0)
    dm=d.get('scheduling',{}).get('materialized_candidate_edges',0)
    if cm>dm:
        semantic_problems.append((key,'materialized_candidate_edges',(cm,dm)))
    if c.get('scheduling',{}).get('scheduled_dependencies',0)>d.get('scheduling',{}).get('scheduled_dependencies',0):
        semantic_problems.append((key,'compact_ready_dag_larger',(
            c.get('scheduling',{}).get('scheduled_dependencies',0),
            d.get('scheduling',{}).get('scheduled_dependencies',0))))
    strict_reductions += int(cm<dm)

    for field in ('positive_observations','negative_observations'):
        cv=c.get('feedback',{}).get(field,0); dv=d.get('feedback',{}).get(field,0)
        delta=abs(cv-dv); max_drift[field]=max(max_drift[field],delta)
        if delta: feedback_drift_pairs.add(key)
    for field in ('mean_probability_q16','mean_confidence_q16'):
        cv=c.get('adaptive_state',{}).get(field,0); dv=d.get('adaptive_state',{}).get(field,0)
        delta=abs(cv-dv); max_drift[field]=max(max_drift[field],delta)
        if delta: posterior_drift_pairs.add(key)
    for field in ('wave_count','max_wave_width','scheduled_dependencies'):
        cv=c.get('scheduling',{}).get(field,0); dv=d.get('scheduling',{}).get(field,0)
        delta=abs(cv-dv); max_drift[field]=max(max_drift[field],delta)
        if delta: schedule_drift_pairs.add(key)

print(f'verification_records={len(records)}')
print(f'verification_pairs={len(pairs)}')
print(f'strict_materialization_reductions={strict_reductions}')
print(f'semantic_mismatches={len(semantic_problems)}')
print(
    'observed_parallel_path_drift: '
    f'feedback_pairs={len(feedback_drift_pairs)}/{len(pairs)} '
    f'posterior_pairs={len(posterior_drift_pairs)}/{len(pairs)} '
    f'schedule_pairs={len(schedule_drift_pairs)}/{len(pairs)} '
    f'max_positive_delta={max_drift["positive_observations"]} '
    f'max_negative_delta={max_drift["negative_observations"]} '
    f'max_probability_q16_delta={max_drift["mean_probability_q16"]} '
    f'max_confidence_q16_delta={max_drift["mean_confidence_q16"]} '
    f'max_wave_delta={max_drift["wave_count"]} '
    f'max_ready_dag_delta={max_drift["scheduled_dependencies"]}'
)
if semantic_problems:
    for problem in semantic_problems[:20]:
        print('  ',problem)
    raise SystemExit(1)
if strict_reductions!=len(pairs):
    raise SystemExit(
        f'FAIL: expected strict materialization reduction in all {len(pairs)} verification pairs, got {strict_reductions}'
    )
print('PASS: compact/dense pairs preserve logical classification and canonical correctness; parallel feedback/posterior drift is reported, not treated as a semantic invariant')
PY

echo "artifacts: $VERIFY"
