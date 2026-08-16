#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BACKUP="$OUT/.pre-retained-gas-rerun/$STAMP"
MAX_GAS="18446744073709551615"

campaigns=(
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

python3 - "$ROOT" "$MAX_GAS" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1]); max_gas=sys.argv[2]
files=sorted((root/'evaluation/conflictlab').glob('v1-*.grid.json'))
problems=[]
for path in files:
    doc=json.load(open(path, encoding='utf-8'))
    base=doc.get('base_run',{}).get('parameters',{})
    if base.get('vm_instance_lifecycle')!='reuse':
        problems.append(f'{path.name}: base lifecycle={base.get("vm_instance_lifecycle")!r}')
    if base.get('vm_gas_limit')!=max_gas:
        problems.append(f'{path.name}: base vm_gas_limit={base.get("vm_gas_limit")!r}')
    vals=doc.get('matrix',{}).get('parameters',{}).get('vm_instance_lifecycle')
    if path.name=='v1-vm-lifecycle.grid.json':
        if set(vals or []) != {'reuse','recycle'}:
            problems.append(f'{path.name}: lifecycle control matrix={vals!r}')
    elif vals is not None and set(vals) != {'reuse'}:
        problems.append(f'{path.name}: lifecycle matrix={vals!r}')
if problems:
    raise SystemExit('retained-VM V1 configuration problems:\n  '+'\n  '.join(problems))
print(f'validated {len(files)} V1 grids: retained canonical reuse + non-binding cumulative gas')
PY

echo "ConflictLab V1 retained-VM rerun will replace cached records collected with the old cumulative gas budget."
echo "The old dataset will be preserved under:"
echo "  $BACKUP"
printf '  %s\n' "${campaigns[@]}"

if [[ "$MODE" == "--dry-run" ]]; then
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

mkdir -p "$BACKUP/campaigns" "$BACKUP/top-level"
for name in "${campaigns[@]}"; do
  if [[ -d "$OUT/$name" ]]; then
    mv "$OUT/$name" "$BACKUP/campaigns/$name"
  fi
done

for path in \
  "$OUT/records.jsonl" \
  "$OUT/validation.txt" \
  "$OUT/results-summary.txt" \
  "$OUT/paper-analysis.md" \
  "$OUT/summary-run.log" \
  "$OUT/campaign-counts.json" \
  "$OUT/suite-environment.txt"; do
  [[ -e "$path" ]] && mv "$path" "$BACKUP/top-level/"
done
if [[ -d "$OUT/aggregate" ]]; then
  mv "$OUT/aggregate" "$BACKUP/top-level/aggregate"
fi
shopt -s nullglob
for path in "$OUT"/acceptance-*.json "$OUT"/*-run.log; do
  mv "$path" "$BACKUP/top-level/"
done
shopt -u nullglob

cat > "$BACKUP/README.txt" <<EOF2
This backup contains the pre-fix ConflictLab V1 dataset. The retained CosmWasm Instance path shared
one cumulative gas meter per retained instance. Heavy compute eventually exhausted that meter and
made serial/adaptive execution diverge because they distribute calls across retained instances
differently. The replacement V1 configuration keeps retained VM reuse for performance comparability
but makes cumulative gas non-binding with vm_gas_limit=$MAX_GAS and validates retained-vs-fresh
canonical state on the previously failing stress identities.
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF2

echo "=== starting retained-VM/non-binding-gas ConflictLab V1 full suite ==="
"$ROOT/scripts/run-conflictlab-v1-evaluation.sh" "$OUT" full
