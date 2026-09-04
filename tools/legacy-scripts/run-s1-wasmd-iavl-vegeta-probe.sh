#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S1_FULL_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
BASE_SERIAL_DIR="${VEGETA_S1_FULL_OUTPUT_DIR:-benchmark-results/wasmd-s1-full-domain-w2}"
WORKERS="${VEGETA_S1_FULL_WORKERS:-2}"
BLOCKS="${VEGETA_S1_IAVL_PROBE_BLOCKS:-100}"
PROBE_ROOT="${VEGETA_S1_IAVL_PROBE_DIR:-benchmark-results/wasmd-s1-iavl-probe-w${WORKERS}}"
SERIAL_DIR="$PROBE_ROOT/serial"
VEGETA_DIR="$PROBE_ROOT/vegeta"
CACHE_SIZE="${VEGETA_S1_IAVL_PROBE_CACHE_SIZE:-0}"
SYNC_PRUNING="${VEGETA_S1_IAVL_PROBE_SYNC_PRUNING:-1}"
BASE_SERIAL="$BASE_SERIAL_DIR/raw/records-w${WORKERS}-serial.jsonl"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl" "$BASE_SERIAL"; do
  [[ -s "$p" ]] || { echo "missing probe prerequisite: $p" >&2; exit 2; }
done
[[ "$WORKERS" =~ ^[1-9][0-9]*$ ]] || { echo "workers must be positive" >&2; exit 2; }
[[ "$BLOCKS" =~ ^[1-9][0-9]*$ ]] || { echo "probe blocks must be positive" >&2; exit 2; }
[[ "$CACHE_SIZE" =~ ^[0-9]+$ ]] || { echo "cache size must be non-negative" >&2; exit 2; }

read -r SCALE ITER_PER_NS <<<"$(python3 - "$BASE_SERIAL" "$BLOCKS" <<'PY'
import json, math, sys
p, want = sys.argv[1], int(sys.argv[2])
scale = it = None
n = 0
for line in open(p, encoding='utf-8'):
    if not line.strip():
        continue
    r=json.loads(line)
    s=float(r.get('compute_scale',0)); i=float(r.get('go_iterations_per_nano',0))
    if not (math.isfinite(s) and s > 0 and math.isfinite(i) and i > 0):
        raise SystemExit('base serial oracle lacks calibration')
    if scale is None:
        scale, it = s, i
    elif scale != s or it != i:
        raise SystemExit('base serial oracle calibration changes within prefix')
    n += 1
    if n == want:
        break
if n != want:
    raise SystemExit(f'base serial oracle rows={n} expected={want}')
print(f'{scale:g} {it:.17g}')
PY
)"

echo "S1 IAVL isolation probe"
echo "  blocks=$BLOCKS workers=$WORKERS"
echo "  iavl_cache_size=$CACHE_SIZE sync_pruning=$SYNC_PRUNING"
echo "  calibration: scale=$SCALE iter/ns=$ITER_PER_NS"
echo "  serial output:  $SERIAL_DIR"
echo "  vegeta output:  $VEGETA_DIR"

rm -rf "$SERIAL_DIR" "$VEGETA_DIR"
mkdir -p "$SERIAL_DIR" "$VEGETA_DIR"

export VEGETA_WASMD_IAVL_CACHE_SIZE="$CACHE_SIZE"
export VEGETA_WASMD_IAVL_SYNC_PRUNING="$SYNC_PRUNING"
unset EVAL_WASMD_CAMPAIGN_PROFILE_DIR || true

# Produce a fresh matched serial oracle under the exact same IAVL/pruning
# configuration.  This is required because async pruning can bleed commit CPU
# into the next block's execution timer; the old serial timings are not a fair
# baseline for the isolation probe.
export EVAL_WASMD_EXEC_DIR="$EXEC_DIR"
export EVAL_WASMD_OUTPUT_DIR="$SERIAL_DIR"
export EVAL_WASMD_WORKERS="$WORKERS"
export EVAL_WASMD_SAMPLES=1
export EVAL_WASMD_MAX_BLOCKS="$BLOCKS"
export EVAL_WASMD_COMPUTE_SCALE="$SCALE"
export EVAL_WASMD_COMPUTE_BASE_TOTAL_MS="${VEGETA_S1_FULL_COMPUTE_BASE_TOTAL_MS:-1000}"
export EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS"
export EVAL_WASMD_BUILD="${VEGETA_S1_IAVL_PROBE_BUILD:-1}"
export EVAL_WASMD_OVERWRITE=1
export EVAL_WASMD_REQUIRE_CLEAN=0
export EVAL_WASMD_SETUP_CHECK=0
export EVAL_WASMD_ISOLATE_STRATEGIES=1
export EVAL_WASMD_REUSE_ISOLATED_PARTS=0
export EVAL_WASMD_REUSE_WEIGHTS=1
export EVAL_WASMD_REUSE_SETUP_TEMPLATE=1
export EVAL_WASMD_ONLY_STRATEGY=serial

echo
echo "=== matched Serial ($BLOCKS blocks) ==="
time bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh smoke

# The Vegeta-only wrapper uses its own output directory and, with BUILD=0,
# expects the already-built evaluator at that directory's bin path. Reuse the
# exact binary that produced the matched Serial oracle so both phases are
# guaranteed to run identical code without paying for a second build.
SERIAL_BIN="$SERIAL_DIR/bin/wasmd-scheduler-eval"
VEGETA_BIN="$VEGETA_DIR/bin/wasmd-scheduler-eval"
[[ -x "$SERIAL_BIN" ]] || { echo "matched Serial binary missing/not executable: $SERIAL_BIN" >&2; exit 3; }
mkdir -p "$VEGETA_DIR/bin"
ln -sfn "$(realpath "$SERIAL_BIN")" "$VEGETA_BIN"
echo "reusing matched Serial evaluator for Vegeta: $VEGETA_BIN -> $(realpath "$SERIAL_BIN")"

# Reuse the newly built binary and the fresh serial oracle through the existing
# Vegeta-only wrapper.
echo
echo "=== matched Vegeta ($BLOCKS blocks) ==="
VEGETA_S1_FULL_OUTPUT_DIR="$SERIAL_DIR" \
VEGETA_S1_VEGETA_OUTPUT_DIR="$VEGETA_DIR" \
VEGETA_S1_VEGETA_MAX_BLOCKS="$BLOCKS" \
VEGETA_S1_VEGETA_BUILD=0 \
time bash tools/legacy-scripts/run-vegeta-s1-wasmd-vegeta-only.sh

python3 - "$SERIAL_DIR/raw/records-w${WORKERS}-serial.jsonl" "$VEGETA_DIR/raw/records-w${WORKERS}-vegeta.jsonl" <<'PY'
import json, sys
sp, vp = sys.argv[1:]
srows=[json.loads(x) for x in open(sp, encoding='utf-8') if x.strip()]
vrows=[json.loads(x) for x in open(vp, encoding='utf-8') if x.strip()]
serial=sum(int(r.get('strategy_total_nanos',0) or r.get('historical_serial_nanos',0)) for r in srows)
# Direct serial records use strategy_total_nanos; Vegeta records carry the fresh
# matched oracle in historical_serial_nanos as well.
if not serial:
    serial=sum(int(r.get('matched_serial_nanos',0)) for r in srows)
pre=sum(int(r.get('pre_consensus_nanos',0)) for r in vrows)
post=sum(int(r.get('post_consensus_nanos',0)) for r in vrows)
wall=sum(int(r.get('strategy_total_nanos',0)) for r in vrows)
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
