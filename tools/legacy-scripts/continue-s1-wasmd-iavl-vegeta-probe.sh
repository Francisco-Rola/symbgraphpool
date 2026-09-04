#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." 2>/dev/null && pwd || true)"
# When run from Downloads, resolve the repository from the current directory.
if [[ -z "$ROOT" || ! -d "$ROOT/benchmarks" ]]; then
  ROOT="$(pwd)"
fi
cd "$ROOT"

WORKERS="${VEGETA_S1_FULL_WORKERS:-2}"
BLOCKS="${VEGETA_S1_IAVL_PROBE_BLOCKS:-100}"
PROBE_ROOT="${VEGETA_S1_IAVL_PROBE_DIR:-benchmark-results/wasmd-s1-iavl-probe-w${WORKERS}}"
SERIAL_DIR="$PROBE_ROOT/serial"
VEGETA_DIR="$PROBE_ROOT/vegeta"
CACHE_SIZE="${VEGETA_S1_IAVL_PROBE_CACHE_SIZE:-0}"
SYNC_PRUNING="${VEGETA_S1_IAVL_PROBE_SYNC_PRUNING:-1}"
SERIAL_ROWS="$SERIAL_DIR/raw/records-w${WORKERS}-serial.jsonl"
SERIAL_BIN="$SERIAL_DIR/bin/wasmd-scheduler-eval"
VEGETA_BIN="$VEGETA_DIR/bin/wasmd-scheduler-eval"

[[ -s "$SERIAL_ROWS" ]] || { echo "missing completed matched Serial oracle: $SERIAL_ROWS" >&2; exit 2; }
[[ -x "$SERIAL_BIN" ]] || { echo "missing matched Serial evaluator: $SERIAL_BIN" >&2; exit 2; }

python3 - "$SERIAL_ROWS" "$BLOCKS" <<'PY'
import json, sys
p, want = sys.argv[1], int(sys.argv[2])
rows = [json.loads(x) for x in open(p, encoding='utf-8') if x.strip()]
if len(rows) != want:
    raise SystemExit(f"matched Serial rows={len(rows)} expected={want}")
if any(r.get('strategy') != 'cosmos-wasmd-direct-serial' for r in rows):
    raise SystemExit('matched Serial oracle contains non-serial rows')
print(f"reusing completed matched Serial: rows={len(rows)}")
PY

mkdir -p "$VEGETA_DIR/bin"
ln -sfn "$(realpath "$SERIAL_BIN")" "$VEGETA_BIN"
echo "reusing exact matched evaluator: $VEGETA_BIN -> $(realpath "$SERIAL_BIN")"

export VEGETA_WASMD_IAVL_CACHE_SIZE="$CACHE_SIZE"
export VEGETA_WASMD_IAVL_SYNC_PRUNING="$SYNC_PRUNING"
unset EVAL_WASMD_CAMPAIGN_PROFILE_DIR || true

echo
echo "=== matched Vegeta continuation ($BLOCKS blocks) ==="
echo "  iavl_cache_size=$CACHE_SIZE sync_pruning=$SYNC_PRUNING"
VEGETA_S1_FULL_OUTPUT_DIR="$SERIAL_DIR" \
VEGETA_S1_VEGETA_OUTPUT_DIR="$VEGETA_DIR" \
VEGETA_S1_VEGETA_MAX_BLOCKS="$BLOCKS" \
VEGETA_S1_VEGETA_BUILD=0 \
time bash tools/legacy-scripts/run-vegeta-s1-wasmd-vegeta-only.sh

VEGETA_ROWS="$VEGETA_DIR/raw/records-w${WORKERS}-vegeta.jsonl"
python3 - "$SERIAL_ROWS" "$VEGETA_ROWS" <<'PY'
import json, sys
sp, vp = sys.argv[1:]
srows=[json.loads(x) for x in open(sp, encoding='utf-8') if x.strip()]
vrows=[json.loads(x) for x in open(vp, encoding='utf-8') if x.strip()]
serial=sum(int(r.get('strategy_total_nanos',0) or r.get('historical_serial_nanos',0) or 0) for r in srows)
pre=sum(int(r.get('pre_consensus_nanos',0) or 0) for r in vrows)
post=sum(int(r.get('post_consensus_nanos',0) or 0) for r in vrows)
wall=sum(int(r.get('strategy_total_nanos',0) or 0) for r in vrows)
S=lambda n:n/1e9
print('\n=== matched result ===')
print(f'rows   = {len(vrows)}')
print(f'serial = {S(serial):.3f}s')
print(f'pre    = {S(pre):.3f}s')
print(f'post   = {S(post):.3f}s')
print(f'wall   = {S(wall):.3f}s')
print(f'post-x = {serial/post:.3f}x' if post else 'post-x = n/a')
print(f'net-x  = {serial/wall:.3f}x' if wall else 'net-x = n/a')
PY
