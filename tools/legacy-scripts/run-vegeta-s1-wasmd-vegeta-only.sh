#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S1_FULL_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
CAL_DIR="${VEGETA_S1_CALIBRATION_DIR:-benchmark-results/wasmd-s1-compute-calibration}"
FULL_OUT_DIR="${VEGETA_S1_FULL_OUTPUT_DIR:-benchmark-results/wasmd-s1-full-domain-w2}"
WORKERS="${VEGETA_S1_FULL_WORKERS:-2}"
SAMPLES="${VEGETA_S1_VEGETA_SAMPLES:-1}"
MAX_BLOCKS="${VEGETA_S1_VEGETA_MAX_BLOCKS:-0}"
BASE_TOTAL_MS="${VEGETA_S1_FULL_COMPUTE_BASE_TOTAL_MS:-1000}"
OUT_DIR="${VEGETA_S1_VEGETA_OUTPUT_DIR:-benchmark-results/wasmd-s1-vegeta-only-w${WORKERS}}"

for p in \
  "$EXEC_DIR/execution-manifest.json" \
  "$EXEC_DIR/execution-plan.jsonl" \
  "$FULL_OUT_DIR/raw/records-w${WORKERS}-serial.jsonl"; do
  [[ -s "$p" ]] || { echo "missing Vegeta-only prerequisite: $p" >&2; exit 2; }
done
[[ "$WORKERS" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_FULL_WORKERS must be a positive integer" >&2; exit 2; }
[[ "$SAMPLES" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_VEGETA_SAMPLES must be a positive integer" >&2; exit 2; }
[[ "$MAX_BLOCKS" =~ ^[0-9]+$ ]] || { echo "VEGETA_S1_VEGETA_MAX_BLOCKS must be a non-negative integer" >&2; exit 2; }

read -r PLAN_BLOCKS PLAN_TX <<<"$(python3 - "$EXEC_DIR/execution-manifest.json" <<'PY'
import json, sys
m=json.load(open(sys.argv[1], encoding='utf-8'))
print(int(m['blocks']), int(m['transactions']))
PY
)"
if (( MAX_BLOCKS == 0 )); then
  EXPECTED_BLOCKS="$PLAN_BLOCKS"
else
  EXPECTED_BLOCKS="$MAX_BLOCKS"
fi
(( EXPECTED_BLOCKS <= PLAN_BLOCKS )) || { echo "VEGETA_S1_VEGETA_MAX_BLOCKS exceeds plan blocks=$PLAN_BLOCKS" >&2; exit 2; }

SERIAL_ORACLE_SOURCE="$FULL_OUT_DIR/raw/records-w${WORKERS}-serial.jsonl"
python3 - "$SERIAL_ORACLE_SOURCE" "$WORKERS" "$EXPECTED_BLOCKS" <<'PY'
import json, sys
path, workers, expected = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rows=0
with open(path, encoding='utf-8') as f:
    for line in f:
        if not line.strip():
            continue
        r=json.loads(line)
        if r.get('strategy') != 'cosmos-wasmd-direct-serial':
            raise SystemExit(f"serial oracle contains non-serial row: {r.get('strategy')}")
        if int(r.get('workers', -1)) != workers:
            raise SystemExit(f"serial oracle workers mismatch: got={r.get('workers')} want={workers}")
        if int(r.get('serial_commit_version', 0)) <= 0 or not r.get('serial_commit_hash'):
            raise SystemExit('serial oracle lacks persisted CommitID fields; run the serial strategy once with the persisted-oracle patch')
        rows += 1
        if rows >= expected:
            break
if rows != expected:
    raise SystemExit(f"serial oracle rows={rows} expected={expected}")
print(f"reusing persisted serial oracle: rows={rows} workers={workers}")
PY

# Keep Vegeta debugging isolated from the full campaign directory. The benchmark
# intentionally requires the serial oracle to contain exactly the requested
# sample/block domain. A prefix debug run therefore materializes only that prefix
# instead of symlinking the full 5,000-row oracle (which would fail the strict
# completeness check with rows=5000 expected=100/800).
mkdir -p "$OUT_DIR/raw"
# Reuse the exact evaluator that produced the persisted Serial oracle. The
# evaluator itself also validates its SHA and IAVL configuration against every
# oracle row, so a source change triggers the stale-binary rebuild guard and
# then fails closed until Serial is regenerated with the current harness.
SERIAL_BIN_SOURCE="$FULL_OUT_DIR/bin/wasmd-scheduler-eval"
if [[ -x "$SERIAL_BIN_SOURCE" ]]; then
  mkdir -p "$OUT_DIR/bin"
  cp -f "$SERIAL_BIN_SOURCE" "$OUT_DIR/bin/wasmd-scheduler-eval"
  chmod +x "$OUT_DIR/bin/wasmd-scheduler-eval"
  echo "reusing exact Serial evaluator binary: $SERIAL_BIN_SOURCE"
fi
SERIAL_ORACLE_DEBUG="$OUT_DIR/raw/records-w${WORKERS}-serial.jsonl"
python3 - "$SERIAL_ORACLE_SOURCE" "$SERIAL_ORACLE_DEBUG" "$SAMPLES" "$EXPECTED_BLOCKS" <<'PYPREFIX'
import json, os, sys, tempfile
src, dst, samples, expected_blocks = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
counts = [0] * samples
selected = 0
os.makedirs(os.path.dirname(dst), exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix='.serial-oracle-prefix.', dir=os.path.dirname(dst), text=True)
try:
    with os.fdopen(fd, 'w', encoding='utf-8') as out, open(src, encoding='utf-8') as inp:
        for line_no, line in enumerate(inp, 1):
            if not line.strip():
                continue
            r = json.loads(line)
            sample = int(r.get('sample', -1))
            if sample < 0 or sample >= samples or counts[sample] >= expected_blocks:
                continue
            if r.get('strategy') != 'cosmos-wasmd-direct-serial':
                raise SystemExit(f"serial oracle contains non-serial row at line {line_no}: {r.get('strategy')}")
            out.write(line if line.endswith('\n') else line + '\n')
            counts[sample] += 1
            selected += 1
            if all(c == expected_blocks for c in counts):
                break
    missing = [(i, c) for i, c in enumerate(counts) if c != expected_blocks]
    if missing:
        raise SystemExit(f"serial oracle prefix incomplete: counts={counts} expected_per_sample={expected_blocks}")
    os.replace(tmp, dst)  # replaces an old symlink itself; never writes through it
finally:
    if os.path.exists(tmp):
        os.unlink(tmp)
print(f"materialized debug serial oracle: rows={selected} samples={samples} blocks_per_sample={expected_blocks}")
PYPREFIX
if (( MAX_BLOCKS == 0 )); then
  for name in compute-weights.jsonl compute-weights-summary.json compute-weights.meta; do
    if [[ -s "$FULL_OUT_DIR/$name" ]]; then
      cp -f "$FULL_OUT_DIR/$name" "$OUT_DIR/$name"
    fi
  done
fi

# The persisted serial oracle is authoritative for compute calibration. A fresh
# calibrate-only run can legitimately differ with CPU frequency / host load, but
# non-serial campaigns must use the exact scale + iter/ns embedded in the oracle
# because the benchmark records compare against those historical serial timings.
read -r ORACLE_SCALE ITER_PER_NS <<<"$(python3 - "$SERIAL_ORACLE_SOURCE" "$EXPECTED_BLOCKS" <<'PYORACLE'
import json, math, sys
path, expected = sys.argv[1], int(sys.argv[2])
scale = None
iter_per_ns = None
rows = 0
with open(path, encoding='utf-8') as f:
    for line_no, line in enumerate(f, 1):
        if not line.strip():
            continue
        r = json.loads(line)
        s = float(r.get('compute_scale', 0))
        i = float(r.get('go_iterations_per_nano', 0))
        if not math.isfinite(s) or s <= 0 or not math.isfinite(i) or i <= 0:
            raise SystemExit(f"serial oracle lacks valid calibration at line {line_no}: scale={s} iter/ns={i}")
        if scale is None:
            scale, iter_per_ns = s, i
        elif s != scale or i != iter_per_ns:
            raise SystemExit(
                f"serial oracle calibration changes within requested prefix at line {line_no}: "
                f"first scale={scale:g} iter/ns={iter_per_ns:.9g}; "
                f"got scale={s:g} iter/ns={i:.9g}"
            )
        rows += 1
        if rows >= expected:
            break
if rows != expected:
    raise SystemExit(f"serial oracle calibration rows={rows} expected={expected}")
print(f"{scale:g} {iter_per_ns:.17g}")
PYORACLE
)"

echo "Vegeta-only calibration inherited from serial oracle: scale=$ORACLE_SCALE iter/ns=$ITER_PER_NS"

export EVAL_WASMD_EXEC_DIR="$EXEC_DIR"
export EVAL_WASMD_OUTPUT_DIR="$OUT_DIR"
export EVAL_WASMD_WORKERS="$WORKERS"
export EVAL_WASMD_SAMPLES="$SAMPLES"
export EVAL_WASMD_MAX_BLOCKS="$MAX_BLOCKS"
export EVAL_WASMD_COMPUTE_SCALE="$ORACLE_SCALE"
export EVAL_WASMD_COMPUTE_BASE_TOTAL_MS="$BASE_TOTAL_MS"
export EVAL_WASMD_BUILD="${VEGETA_S1_VEGETA_BUILD:-0}"
export EVAL_WASMD_OVERWRITE=1
export EVAL_WASMD_REQUIRE_CLEAN=0
export EVAL_WASMD_SETUP_CHECK=0
export EVAL_WASMD_ISOLATE_STRATEGIES=1
export EVAL_WASMD_REUSE_ISOLATED_PARTS=0
export EVAL_WASMD_REUSE_WEIGHTS=1
export EVAL_WASMD_REUSE_SETUP_TEMPLATE=1
export EVAL_WASMD_ONLY_STRATEGY=vegeta
export EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS"

echo "Vegeta-only S1 Wasmd debug: blocks=$EXPECTED_BLOCKS/$PLAN_BLOCKS workers=$WORKERS samples=$SAMPLES"
echo "serial/blockstm/other schedulers will NOT run"
echo "debug output: $OUT_DIR"
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh smoke

VEGETA_PART="$OUT_DIR/raw/records-w${WORKERS}-vegeta.jsonl"
python3 - "$VEGETA_PART" "$WORKERS" "$EXPECTED_BLOCKS" <<'PYVALIDATE'
import json, sys
path, workers, expected = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rows = attempts = reexec = safety = fallbacks = 0
with open(path, encoding='utf-8') as f:
    for line in f:
        if not line.strip():
            continue
        r = json.loads(line)
        if r.get('strategy') != 'cosmos-wasmd-vegeta':
            raise SystemExit(f"unexpected strategy row: {r.get('strategy')}")
        if int(r.get('workers', -1)) != workers:
            raise SystemExit('workers mismatch')
        if not bool(r.get('serial_equivalent')):
            raise SystemExit(f"non-serial-equivalent Vegeta row at block {r.get('block_number')}")
        attempts += int(r.get('execution_attempts', 0) or 0)
        reexec += int(r.get('reexecutions', 0) or 0)
        safety += int(r.get('safety_replays', 0) or 0)
        fallbacks += int(r.get('forward_fallbacks', 0) or 0)
        rows += 1
if rows != expected:
    raise SystemExit(f"Vegeta rows={rows} expected={expected}")
pct = (100.0 * reexec / max(1, attempts - reexec)) if attempts else 0.0
print(
    f"PASS: Vegeta-only serial-equivalent validation rows={rows} workers={workers} "
    f"attempts={attempts} reexec={reexec} reexec_pct={pct:.2f}% "
    f"safety_replays={safety} forward_fallbacks={fallbacks}"
)
PYVALIDATE

echo "raw: $VEGETA_PART"
