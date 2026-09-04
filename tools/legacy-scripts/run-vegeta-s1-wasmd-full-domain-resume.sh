#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S1_FULL_EXEC_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
CAL_DIR="${VEGETA_S1_CALIBRATION_DIR:-benchmark-results/wasmd-s1-compute-calibration}"
OUT_DIR="${VEGETA_S1_FULL_OUTPUT_DIR:-benchmark-results/wasmd-s1-full-domain-w2}"
WORKERS="${VEGETA_S1_FULL_WORKERS:-2}"
SAMPLES="${VEGETA_S1_FULL_SAMPLES:-1}"
SCALE="${VEGETA_S1_FULL_COMPUTE_SCALE:-4}"
BASE_TOTAL_MS="${VEGETA_S1_FULL_COMPUTE_BASE_TOTAL_MS:-1000}"

for p in \
  "$EXEC_DIR/execution-manifest.json" \
  "$EXEC_DIR/execution-plan.jsonl" \
  "$CAL_DIR/calibration-summary.json"; do
  [[ -s "$p" ]] || { echo "missing S1 full-domain prerequisite: $p" >&2; exit 2; }
done
[[ "$WORKERS" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_FULL_WORKERS must be a positive integer" >&2; exit 2; }
[[ "$SAMPLES" =~ ^[1-9][0-9]*$ ]] || { echo "VEGETA_S1_FULL_SAMPLES must be a positive integer" >&2; exit 2; }
[[ "$SCALE" == "4" || "$SCALE" == "4.0" ]] || {
  echo "S1 publication candidate is frozen to calibrated gas_used scale 4 for this validation wrapper" >&2
  exit 2
}

FIAT_MINT_TOOL="tools/vegeta/repair-s1-fiat-token-mint-senders.py"
FIAT_MINT_REPORT="$EXEC_DIR/fiat-token-mint-sender-repair.json"
fiat_mint_cache_ok() {
  [[ -s "$FIAT_MINT_REPORT" ]] || return 1
  [[ ! "$EXEC_DIR/execution-plan.jsonl" -nt "$FIAT_MINT_REPORT" ]] || return 1
  [[ ! "$FIAT_MINT_TOOL" -nt "$FIAT_MINT_REPORT" ]] || return 1
  python3 - "$FIAT_MINT_REPORT" <<'PY' >/dev/null
import json, sys
d=json.load(open(sys.argv[1], encoding='utf-8'))
assert d.get('status') in {'already-complete','repaired-and-validated'}
assert int(d.get('mismatched_mints', -1)) == 0
PY
}

if fiat_mint_cache_ok; then
  echo "reusing cached S1 FiatToken mint authorization repair: PASS"
else
  echo "preflighting committed S1 FiatToken mint authorization before Wasmd"
  python3 "$FIAT_MINT_TOOL" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --report "$FIAT_MINT_REPORT"
fi

LIFECYCLE_TOOL="tools/vegeta/repair-s1-cw721-lifecycle-from-mint-logs.py"
LIFECYCLE_REPORT="$EXEC_DIR/cw721-lifecycle-repair.json"
lifecycle_cache_ok() {
  [[ -s "$LIFECYCLE_REPORT" ]] || return 1
  [[ ! "$EXEC_DIR/execution-plan.jsonl" -nt "$LIFECYCLE_REPORT" ]] || return 1
  [[ ! "$EXEC_DIR/execution-manifest.json" -nt "$LIFECYCLE_REPORT" ]] || return 1
  [[ ! "$LIFECYCLE_TOOL" -nt "$LIFECYCLE_REPORT" ]] || return 1
  python3 - "$LIFECYCLE_REPORT" <<'PY' >/dev/null
import json, sys
d=json.load(open(sys.argv[1], encoding='utf-8'))
assert d.get('status') in {'already-complete','repaired-and-validated'}
assert int(d.get('blocks',-1)) == 5000
assert int(d.get('transactions',-1)) == 739863
assert int(d.get('remaining_missing_tokens',-1)) == 0
PY
}

if lifecycle_cache_ok; then
  echo "reusing cached full-domain NFT lifecycle validation: PASS"
else
  echo "preflighting full-domain committed NFT lifecycle before Wasmd"
  python3 "$LIFECYCLE_TOOL" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --report "$LIFECYCLE_REPORT"
fi

CW721_OWNER_TOOL="tools/vegeta/repair-s1-cw721-ownership-from-transfer-logs.py"
CW721_OWNER_REPORT="$EXEC_DIR/cw721-ownership-repair.json"
cw721_owner_cache_ok() {
  [[ -s "$CW721_OWNER_REPORT" ]] || return 1
  [[ ! "$EXEC_DIR/execution-plan.jsonl" -nt "$CW721_OWNER_REPORT" ]] || return 1
  [[ ! "$EXEC_DIR/execution-manifest.json" -nt "$CW721_OWNER_REPORT" ]] || return 1
  [[ ! "$CW721_OWNER_TOOL" -nt "$CW721_OWNER_REPORT" ]] || return 1
  python3 - "$CW721_OWNER_REPORT" <<'PY' >/dev/null
import json, sys
d=json.load(open(sys.argv[1], encoding='utf-8'))
assert d.get('status') in {'already-complete','repaired-and-validated'}
assert int(d.get('blocks',-1)) == 5000
assert int(d.get('transactions',-1)) == 739863
assert int(d.get('remaining_owner_gaps',-1)) == 0
PY
}

if cw721_owner_cache_ok; then
  echo "reusing cached full-domain CW721 ownership validation: PASS"
else
  echo "preflighting full-domain committed CW721 ownership before Wasmd"
  python3 "$CW721_OWNER_TOOL" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --report "$CW721_OWNER_REPORT"
  # Ownership recovery only adds ordinary transfers of already-existing tokens. Refresh the
  # lifecycle report against the final plan so future resumes do not treat it as stale; this is
  # an offline scan because ownership recovery cannot create a token-existence gap.
  python3 "$LIFECYCLE_TOOL" \
    --execution-plan "$EXEC_DIR/execution-plan.jsonl" \
    --manifest "$EXEC_DIR/execution-manifest.json" \
    --report "$LIFECYCLE_REPORT"
fi

read -r PLAN_BLOCKS PLAN_TX <<<"$(python3 - "$EXEC_DIR/execution-manifest.json" <<'PY'
import json, sys
m=json.load(open(sys.argv[1], encoding='utf-8'))
print(int(m['blocks']), int(m['transactions']))
PY
)"
[[ "$PLAN_BLOCKS" == "5000" && "$PLAN_TX" == "739863" ]] || {
  echo "unexpected S1 execution domain: blocks=$PLAN_BLOCKS tx=$PLAN_TX" >&2
  exit 2
}

python3 - "$CAL_DIR/calibration-summary.json" <<'PY'
import json, sys
p=sys.argv[1]
d=json.load(open(p, encoding='utf-8'))
assert d.get('blocks') == 101
w=d.get('weights', {})
assert w.get('metric') == 'gas_used'
assert int(w.get('missing', -1)) == 0
assert bool(w.get('relative_distribution_invariant_across_scales'))
c=d.get('candidate_4x', {})
assert float(c.get('scale')) == 4.0
assert bool(c.get('same_top_strategy_across_2x_4x_8x'))
profiles={float(x['scale']): x for x in d.get('profiles', [])}
assert {1.0,2.0,4.0,8.0}.issubset(profiles)
share=float(profiles[4.0]['fitted_supplement_share'])
print(f"calibration accepted: gas_used scale=4 fitted supplemental share={share:.1%}; top strategy stable across 2x/4x/8x")
PY

GO_BENCH_DIR="benchmarks/cosmos-wasmd-blockstm-s3"
NATIVE_CONTRACT_ROOTS=("benchmarks/contracts/native-s3" "benchmarks/contracts/native-s1")
binary_and_wasm_sources_fresh() {
  local candidate="$1"
  [[ -x "$candidate" ]] || return 1
  if find "$GO_BENCH_DIR" -type f -name '*.go' -newer "$candidate" -print -quit | grep -q .; then
    return 1
  fi
  if find "${NATIVE_CONTRACT_ROOTS[@]}" -type f \
      \( -name '*.rs' -o -name 'Cargo.toml' \) -newer "$candidate" -print -quit | grep -q .; then
    return 1
  fi
}

BIN=""
for candidate in \
  "$CAL_DIR/scale-4/bin/wasmd-scheduler-eval" \
  "benchmark-results/wasmd-s1-smoke/bin/wasmd-scheduler-eval" \
  "$CAL_DIR/bin/wasmd-scheduler-eval"; do
  if binary_and_wasm_sources_fresh "$candidate"; then
    BIN="$candidate"
    break
  fi
done

mkdir -p "$OUT_DIR/bin"
if [[ -n "$BIN" ]]; then
  BIN_ABS="$(realpath "$BIN")"
  ln -sfn "$BIN_ABS" "$OUT_DIR/bin/wasmd-scheduler-eval"
  echo "reusing fresh calibrated Wasmd scheduler binary: $BIN"
  BUILD=0
else
  echo "scheduler binary or native Wasm sources changed; rebuilding once"
  BUILD=1
fi

ITER_SOURCE="$CAL_DIR/iterations-per-ns.txt"
if [[ "$BUILD" == "0" && -s "$ITER_SOURCE" ]] && grep -Eq '^[0-9]+([.][0-9]+)?$' "$ITER_SOURCE"; then
  ITER_PER_NS="$(cat "$ITER_SOURCE")"
  printf '%s\n' "$ITER_PER_NS" > "$OUT_DIR/iterations-per-ns.txt"
  echo "reusing pinned calibration iter/ns=$ITER_PER_NS"
else
  ITER_PER_NS=""
fi

# The aggregate records file is regenerated on every resume, while completed isolated
# strategy parts are retained and validated by scripts/eval-wasmd.sh.  With one sample,
# an interruption can therefore lose at most the currently running strategy, not all
# previously completed strategies or any upstream S1 preparation work.
export EVAL_WASMD_EXEC_DIR="$EXEC_DIR"
export EVAL_WASMD_OUTPUT_DIR="$OUT_DIR"
export EVAL_WASMD_WORKERS="$WORKERS"
export EVAL_WASMD_SAMPLES="$SAMPLES"
export EVAL_WASMD_MAX_BLOCKS=0
export EVAL_WASMD_COMPUTE_SCALE=4
export EVAL_WASMD_COMPUTE_BASE_TOTAL_MS="$BASE_TOTAL_MS"
export EVAL_WASMD_BUILD="$BUILD"
export EVAL_WASMD_OVERWRITE=1
export EVAL_WASMD_REQUIRE_CLEAN=0
export EVAL_WASMD_SETUP_CHECK=0
export EVAL_WASMD_ISOLATE_STRATEGIES=1
export EVAL_WASMD_REUSE_ISOLATED_PARTS=1
export EVAL_WASMD_REUSE_WEIGHTS=1
export EVAL_WASMD_REUSE_SETUP_TEMPLATE=1
if [[ -n "$ITER_PER_NS" ]]; then
  export EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS"
fi

printf '%s\n' \
  "selection=gas_used-scale-4" \
  "calibration=$CAL_DIR/calibration-summary.json" \
  "blocks=$PLAN_BLOCKS" \
  "transactions=$PLAN_TX" \
  "workers=$WORKERS" \
  "samples=$SAMPLES" \
  > "$OUT_DIR/s1-full-domain-selection.txt"

# Explicit MAX_BLOCKS=0 overrides smoke's 101-block default while retaining its
# one-worker-setting/one-sample evaluation defaults supplied above.
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh smoke

python3 - "$OUT_DIR/summary/summary.csv" "$WORKERS" <<'PY'
import csv, sys
rows=list(csv.DictReader(open(sys.argv[1], encoding='utf-8')))
w=int(sys.argv[2])
expected={
    'cosmos-wasmd-direct-serial',
    'cosmos-wasmd-block-stm',
    'cosmos-wasmd-aria-fb',
    'cosmos-wasmd-vegeta',
    'cosmos-wasmd-symbgraph-rust',
}
chosen=[r for r in rows if int(r['workers']) == w]
got={r['strategy'] for r in chosen}
if got != expected:
    raise SystemExit(f"missing full-domain strategies: expected={sorted(expected)} got={sorted(got)}")
if not all(str(r.get('serial_equivalent','')).lower() in {'true','1'} for r in chosen):
    raise SystemExit('full-domain campaign contains a non-serial-equivalent strategy result')
print('PASS: all five S1 strategies are serial-equivalent across the full 5000-block domain')
PY

echo
echo "PASS: Vegeta S1 full-domain Wasmd validation completed"
echo "domain: 5000 blocks / 739863 transactions"
echo "calibrated compute: gas_used x4"
echo "summary: $OUT_DIR/summary/summary.txt"
echo "resume: rerun this same command; completed isolated strategies are reused"
