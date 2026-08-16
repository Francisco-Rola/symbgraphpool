#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BACKUP="$OUT/.pre-compaction-reference-window-fix/$STAMP"
CAMPAIGN="compaction-reference"

python3 - "$ROOT" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1])
path=root/'evaluation/conflictlab/v1-compaction-reference.grid.json'
doc=json.load(open(path, encoding='utf-8'))
base=doc.get('base_run',{}).get('parameters',{})
if base.get('consensus_cutoff_ms') != '5000':
    raise SystemExit('expected compaction reference consensus_cutoff_ms=5000')
vals=doc.get('matrix',{}).get('parameters',{}).get('acg.compact_equivalence_groups',[])
if set(vals) != {'true','false'}:
    raise SystemExit(f'expected compact/dense pair axis, found {vals}')
print('verified compaction-reference semantic window: 5000 ms, compact+dense paired')
PY

if [[ "$MODE" == "--dry-run" ]]; then
  echo "Would force rerun only: $CAMPAIGN (240 runs)"
  echo "The other 14 V1 campaigns will be reused by the normal full-suite runner."
  echo "Old compaction-reference artifacts would be preserved under:"
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
This backup contains the pre-fix compaction-reference campaign and combined V1 postprocessing.
The old reference used a 250 ms consensus cutoff. Because compaction changes planning wall time,
compact and dense warm-up blocks could expose slightly different execution-evidence populations
before cutoff, which then perturbed probability-only feedback/posteriors. The replacement uses a
non-binding 5000 ms reference window and validation requires complete preexecution before exact
logical feedback/posterior equality is compared.
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF2

echo '=== rerunning ConflictLab V1 with only compaction-reference invalidated ==='
"$ROOT/scripts/run-conflictlab-v1-evaluation.sh" "$OUT" full
