#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/benchmark-results/conflictlab-v1-core}"
MODE="${2:-run}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BACKUP="$OUT/.pre-compaction-reference-normalized-warmup/$STAMP"
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
if base.get('acg.warmup_compact_equivalence_groups') != 'false':
    raise SystemExit('expected compaction reference dense normalized warm-up')
if base.get('acg.warmup_workers') != '1':
    raise SystemExit('expected compaction reference deterministic single-worker warm-up')
vals=doc.get('matrix',{}).get('parameters',{}).get('acg.compact_equivalence_groups',[])
if set(vals) != {'true','false'}:
    raise SystemExit(f'expected compact/dense pair axis, found {vals}')
print('verified compaction-reference design: dense single-worker warm-up, 5000 ms measured window, compact+dense paired')
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
The previous reference allowed compact and dense runs to train their adaptive posterior under
different physical representations and then, after that was normalized, still executed the dense
warm-up trajectory independently on six workers. Parallel warm-up execution could produce small
feedback/reconciliation differences before the measured toggle. The replacement trains both halves
with dense materialization and a deterministic single-worker speculative executor for all warm-up
blocks, restores each run's compact/dense toggle only for the measured 6-worker block, and retains
the non-binding 5000 ms reference window. Exact feedback/posterior equality is therefore compared
from a deterministic common training procedure.
created_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF2

echo '=== rerunning ConflictLab V1 with only compaction-reference invalidated ==='
"$ROOT/scripts/run-conflictlab-v1-evaluation.sh" "$OUT" full
