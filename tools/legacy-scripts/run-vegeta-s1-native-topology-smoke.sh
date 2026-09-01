#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BLOCKS="${VEGETA_S1_TOPOLOGY_BLOCKS:-101}"
if [[ "$BLOCKS" != "101" ]]; then
  echo "this fast wrapper currently requires 101 blocks because the native access executor retains the frozen S3 101-block completion guard" >&2
  exit 2
fi

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
EXEC_DIR="${VEGETA_S1_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
OUT_DIR="${VEGETA_S1_TOPOLOGY_OUT_DIR:-$EXEC_DIR/topology-smoke-101}"
MANIFEST="$EXEC_DIR/execution-manifest.json"
EXEC_PLAN="$EXEC_DIR/execution-plan.jsonl"
NATIVE_PLAN="$PLAN_DIR/native-plan.jsonl"
PREFIX_CORPUS="$OUT_DIR/source-corpus-101.jsonl"
PREFIX_PLAN="$OUT_DIR/execution-plan-101.jsonl"
PREFIX_NATIVE_PLAN="$OUT_DIR/native-plan-101.jsonl"
NATIVE_ACCESSES="$OUT_DIR/native-accesses.jsonl"
FIDELITY_JSON="$OUT_DIR/native-topology-fidelity.json"
FIDELITY_TXT="$OUT_DIR/native-topology-fidelity.txt"
ATTRIBUTION_JSON="$OUT_DIR/native-topology-attribution.json"
EXECUTOR="runtime/target/release/acg-vegeta-native-s3-executor"
MEASURE="tools/vegeta/measure-native-s3-fidelity.py"

for p in "$CORPUS" "$NATIVE_PLAN" "$MANIFEST" "$EXEC_PLAN" "$MEASURE"; do
  [[ -s "$p" ]] || { echo "missing required S1 topology input: $p" >&2; exit 2; }
done
mkdir -p "$OUT_DIR"

update_prefix() {
  local src="$1" dst="$2" tmp="$2.tmp"
  head -n "$BLOCKS" "$src" > "$tmp"
  if [[ -f "$dst" ]] && cmp -s "$tmp" "$dst"; then
    rm -f "$tmp"
  else
    mv "$tmp" "$dst"
  fi
}
update_prefix "$CORPUS" "$PREFIX_CORPUS"
update_prefix "$EXEC_PLAN" "$PREFIX_PLAN"
update_prefix "$NATIVE_PLAN" "$PREFIX_NATIVE_PLAN"

python3 - "$PREFIX_CORPUS" "$PREFIX_PLAN" "$PREFIX_NATIVE_PLAN" "$BLOCKS" <<'PY'
import json,sys
from pathlib import Path
c,p,np,n=Path(sys.argv[1]),Path(sys.argv[2]),Path(sys.argv[3]),int(sys.argv[4])
def rows(path):
    return [json.loads(x) for x in path.read_text().splitlines() if x.strip()]
cr,pr,npr=rows(c),rows(p),rows(np)
if len(cr)!=n or len(pr)!=n or len(npr)!=n:
    raise SystemExit(f"prefix size mismatch corpus={len(cr)} execution_plan={len(pr)} native_plan={len(npr)} expected={n}")
cb=[int(x['block_number']) for x in cr]; pb=[int(x['block_number']) for x in pr]; npb=[int(x['block_number']) for x in npr]
if cb!=pb or cb!=npb:
    raise SystemExit("source/execution/native-plan prefix block numbers differ")
print(f"S1 topology prefix ready: blocks={n} first={cb[0]} last={cb[-1]}")
PY

if [[ ! -x "$EXECUTOR" ]] || find runtime/crates/acg-vegeta-native-s3-executor runtime/crates/acg-cosmwasm -type f -newer "$EXECUTOR" -print -quit 2>/dev/null | grep -q .; then
  echo "native access executor missing/stale; building once"
  cargo build --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --release
else
  echo "reusing cached native access executor"
fi

rerun_access=0
if [[ ! -s "$NATIVE_ACCESSES" ]]; then
  rerun_access=1
else
  for dep in "$PREFIX_PLAN" "$MANIFEST" "$EXECUTOR"; do
    [[ "$dep" -nt "$NATIVE_ACCESSES" ]] && rerun_access=1
  done
fi
if (( rerun_access )); then
  echo "collecting concrete native CosmWasm accesses for the 101-block S1 smoke prefix"
  "$EXECUTOR" \
    --repo-root "$ROOT" \
    --manifest "$MANIFEST" \
    --plan "$PREFIX_PLAN" \
    --output "$NATIVE_ACCESSES"
else
  echo "reusing cached 101-block native access audit"
fi

rerun_fidelity=0
for out in "$FIDELITY_JSON" "$FIDELITY_TXT" "$ATTRIBUTION_JSON"; do
  [[ -s "$out" ]] || rerun_fidelity=1
done
if (( ! rerun_fidelity )); then
  for dep in "$PREFIX_CORPUS" "$NATIVE_ACCESSES" "$PREFIX_NATIVE_PLAN" "$MEASURE"; do
    [[ "$dep" -nt "$FIDELITY_JSON" ]] && rerun_fidelity=1
  done
fi
if (( rerun_fidelity )); then
  echo "measuring 101-block topology fidelity using prefix-only plans (memory bounded)"
  python3 "$MEASURE" \
    --dataset vegeta-s1-native-topology-smoke-101 \
    --corpus "$PREFIX_CORPUS" \
    --native-accesses "$NATIVE_ACCESSES" \
    --native-plan "$PREFIX_NATIVE_PLAN" \
    --output-dir "$OUT_DIR"
else
  echo "reusing cached S1 topology fidelity report"
  cat "$FIDELITY_TXT"
fi

echo
echo "PASS: S1 101-block concrete native topology audit completed"
echo "report: $FIDELITY_TXT"
