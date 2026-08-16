#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BACKUP="$OUT/.pre-affected-rerun/$STAMP"
DIAG="$OUT/correctness-diagnostics"

if [[ -s "$OUT/records.jsonl" ]]; then
  if python3 - "$OUT/records.jsonl" <<'PY'
import json,sys
MAX_GAS=str((1<<64)-1)
legacy=[]
for lineno,line in enumerate(open(sys.argv[1], encoding='utf-8'),1):
    if not line.strip():
        continue
    r=json.loads(line)
    p=r.get('metadata',{}).get('parameters',{})
    if p.get('vm_instance_lifecycle') == 'reuse' and p.get('vm_gas_limit') != MAX_GAS:
        legacy.append(lineno)
        if len(legacy) >= 5:
            break
raise SystemExit(0 if legacy else 1)
PY
  then
    echo "Cached V1 records use retained VM reuse with the old binding cumulative gas budget." >&2
    echo "Do not mix those timings with the corrected configuration." >&2
    echo "First verify the 124 historical failures with:" >&2
    echo "  scripts/verify-conflictlab-v1-retained-vm-correctness.sh \"$OUT\"" >&2
    echo "Then migrate the canonical suite with:" >&2
    echo "  scripts/rerun-conflictlab-v1-retained-vm-suite.sh \"$OUT\"" >&2
    exit 2
  fi
fi

python3 "$ROOT/scripts/diagnose-conflictlab-v1-correctness.py" "$OUT" >/dev/null

mapfile -t campaigns < <(python3 - "$DIAG/campaign-status.csv" "$DIAG/rerun-plan.json" <<'PY'
import csv,json,sys
selected=set()
with open(sys.argv[1], newline='', encoding='utf-8') as f:
    for row in csv.DictReader(f):
        status=row.get('acceptance_status')
        failures=int(row.get('correctness_failures') or 0)
        if status != 'accepted' or failures > 0:
            selected.add(row['campaign'])
for item in json.load(open(sys.argv[2], encoding='utf-8')):
    if item.get('reason') == 'unexpected-input-resolved-candidate-miss':
        selected.add(item['campaign'])
for name in sorted(selected):
    print(name)
PY
)

if ((${#campaigns[@]} == 0)); then
  echo "No affected campaigns detected."
  exit 0
fi

echo "Affected campaigns to force-rerun:"
printf '  %s\n' "${campaigns[@]}"
echo "Old campaign directories will be preserved under: $BACKUP"
if [[ "$MODE" == "--dry-run" ]]; then
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi
mkdir -p "$BACKUP"
for name in "${campaigns[@]}"; do
  if [[ -d "$OUT/$name" ]]; then
    mv "$OUT/$name" "$BACKUP/$name"
  fi
done

# The canonical V1 runner will now regenerate only the moved/rejected campaigns and safely reuse
# every remaining accepted campaign whose manifest still matches the current grid.
"$ROOT/scripts/run-conflictlab-v1-evaluation.sh" "$OUT" full
