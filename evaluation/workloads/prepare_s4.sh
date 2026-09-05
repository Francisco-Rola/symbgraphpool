#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CORPUS="$ROOT/benchmarks/corpora/vegeta-ethereum/s4/corpus.jsonl"
[[ -s "$CORPUS" ]] || { echo "S4 corpus not collected yet; run evaluation/workloads/collect_s4.sh" >&2; exit 2; }
cat <<'MSG'
S4 source corpus is present, but the S4 native-family translation is intentionally a placeholder until the current tracer completes and the dominant contract families are characterized.
Target output: benchmarks/corpora/vegeta-ethereum/s4/native-execution/{execution-manifest.json,execution-plan.jsonl,symbolic/}
Once present, evaluation/experiments/02_s4_headline.sh will run unchanged.
MSG
exit 3
