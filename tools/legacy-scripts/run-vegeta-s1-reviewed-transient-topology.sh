#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

BASE="${VEGETA_S1_TOPOLOGY_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution/topology-smoke-101}"
CORPUS="$BASE/source-corpus-101.jsonl"
PLAN="$BASE/execution-plan-101.jsonl"
NATIVE="$BASE/native-accesses.jsonl"
OUT_JSON="$BASE/reviewed-transient-topology.json"
OUT_TXT="$BASE/reviewed-transient-topology.txt"
MEASURE="tools/vegeta/measure-vegeta-s1-reviewed-transient-topology.py"

for p in "$CORPUS" "$PLAN" "$NATIVE" "$MEASURE"; do
  [[ -s "$p" ]] || { echo "missing cached topology input: $p" >&2; echo "run tools/legacy-scripts/run-vegeta-s1-native-topology-smoke.sh first" >&2; exit 2; }
done

rerun=0
for out in "$OUT_JSON" "$OUT_TXT"; do [[ -s "$out" ]] || rerun=1; done
if (( ! rerun )); then
  for dep in "$CORPUS" "$PLAN" "$NATIVE" "$MEASURE"; do [[ "$dep" -nt "$OUT_JSON" ]] && rerun=1; done
fi

if (( rerun )); then
  echo "measuring reviewed transient topology from cached 101-block data (no RPC, no execution)"
  python3 "$MEASURE" \
    --corpus "$CORPUS" \
    --execution-plan "$PLAN" \
    --native-accesses "$NATIVE" \
    --output-json "$OUT_JSON" \
    --output-text "$OUT_TXT"
else
  echo "reusing cached reviewed transient topology diagnostic"
  cat "$OUT_TXT"
fi

echo
echo "PASS: reviewed transient topology diagnostic completed without RPC"
echo "report: $OUT_TXT"
