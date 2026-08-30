#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
OUT_DIR="${VEGETA_S3_PERF_SMOKE_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/perf-smoke}"
PERF_BIN="${VEGETA_S3_PERF_BIN:-$(command -v perf || true)}"
[[ -n "$PERF_BIN" ]] || { echo "ERROR: perf not found; set VEGETA_S3_PERF_BIN" >&2; exit 2; }
mkdir -p "$OUT_DIR"
echo "perf binary: $PERF_BIN"
"$PERF_BIN" --version

SOFTWARE="task-clock,context-switches,cpu-migrations,page-faults"
HARDWARE="$SOFTWARE,cycles,instructions,branches,branch-misses,cache-references,cache-misses"
EVENTS="$HARDWARE"
MODE="hardware+software"
if ! LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$EVENTS" -- true >/dev/null 2>&1; then
  EVENTS="$SOFTWARE"; MODE="software-only"
  LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$EVENTS" -- true >/dev/null 2>&1 || {
    echo "ERROR: perf_event_open is unavailable for even software counters." >&2
    echo "Try: cat /proc/sys/kernel/perf_event_paranoid" >&2
    echo "For a private WSL dev VM only: sudo sysctl -w kernel.perf_event_paranoid=1" >&2
    exit 3
  }
fi

RAW="$OUT_DIR/perf-smoke.csv"
META="$OUT_DIR/perf-smoke.json"
# CPU-bound ~0.4 s loop; avoids depending on a compiled project binary.
started="$(date +%s%N)"
LC_ALL=C "$PERF_BIN" stat --no-big-num -x ';' -e "$EVENTS" -o "$RAW" -- \
  python3 - <<'PY'
x = 0x9E3779B97F4A7C15
for i in range(3_000_000):
    x ^= (x << 7) & ((1 << 64) - 1)
    x ^= x >> 9
    x = (x * 0xBF58476D1CE4E5B9 + i) & ((1 << 64) - 1)
print(hex(x))
PY
ended="$(date +%s%N)"; elapsed=$((ended-started))
python3 - "$RAW" "$META" "$elapsed" "$MODE" <<'PY'
import json,sys
sys.path.insert(0,'tools/vegeta')
from perf_stat import perf_health
raw,out,elapsed,mode=sys.argv[1:]
h=perf_health(raw, int(elapsed)/1e6)
obj={'mode':mode,'elapsed_ms_shell':int(elapsed)/1e6,**h}
json.dump(obj,open(out,'w'),indent=2)
print(json.dumps({k:obj[k] for k in ('mode','task_clock_ms','duration_ms','avg_cpus','ipc','cache_miss_rate','working')},indent=2))
if not h['working']:
    raise SystemExit('FAIL: perf ran but task-clock was not parsed; inspect '+raw)
if h['avg_cpus'] is None or h['avg_cpus'] <= 0.05:
    raise SystemExit('FAIL: perf average CPU estimate is implausible; inspect '+raw)
print('PASS: perf smoke counters are readable and parser-compatible')
PY

echo "raw perf CSV: $RAW"
echo "parsed smoke metadata: $META"
