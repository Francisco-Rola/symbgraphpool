#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BACKUP="$OUT/.pre-resource-granularity-fix/$STAMP"
CAMPAIGN="symbolic-granularity"

python3 - "$ROOT" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1])
affected=[]
for path in sorted((root/'evaluation/conflictlab').glob('v1-*.grid.json')):
    doc=json.load(open(path, encoding='utf-8'))
    vals=[]
    base=doc.get('base_run',{}).get('parameters',{}).get('symbolic_granularity')
    if base: vals.append(base)
    matrix=doc.get('matrix',{}).get('parameters',{}).get('symbolic_granularity')
    if matrix: vals.extend(matrix)
    if any(value in {'resource','profile'} for value in vals):
        affected.append(path.name)
expected=['v1-symbolic-granularity.grid.json']
if affected != expected:
    raise SystemExit(f'resource/profile granularity scope changed; expected {expected}, found {affected}')
print('verified code-change scope: only v1-symbolic-granularity exercises resource/profile granularity')
PY

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would force rerun only: $CAMPAIGN (72 runs)"
  echo "The other 14 V1 campaigns will be reused by the normal full-suite runner."
  echo "Old symbolic-granularity artifacts would be preserved under:"
  echo "  $BACKUP"
  exit 0
fi
if [[ "$MODE" != "run" ]]; then
  echo "usage: $0 [output-dir] [--dry-run]" >&2
  exit 2
fi

if [[ ! -d "$OUT/$CAMPAIGN" ]]; then
  echo "missing cached campaign directory: $OUT/$CAMPAIGN" >&2
  exit 1
fi

mkdir -p "$BACKUP/campaign" "$BACKUP/top-level"
mv "$OUT/$CAMPAIGN" "$BACKUP/campaign/$CAMPAIGN"
for path in \
  "$OUT/acceptance-$CAMPAIGN.json" \
  "$OUT/$CAMPAIGN-run.log" \
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

cat > "$BACKUP/README.txt" <<EOF2
This backup contains the pre-fix symbolic-granularity campaign and combined V1 postprocessing.
The resource granularity transform encoded whole-resource collapse as a scalar unresolved logical
key. Probability-only learning could therefore prune that relationship and create the six
point-mixed candidate-miss records. The replacement encodes resource collapse as a FieldSet so
profile-edge derivation emits KeyMatch::WholeResource, matching the V1 experiment specification.
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF2

echo '=== rerunning ConflictLab V1 with only symbolic-granularity invalidated ==='
"$ROOT/scripts/run-conflictlab-v1-evaluation.sh" "$OUT" full
