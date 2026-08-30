#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXACT_CORPUS="${VEGETA_S3_EXACT_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/corpus.jsonl}"
PUBLIC_CORPUS="${VEGETA_S3_PUBLIC_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
NATIVE_ACCESSES="${VEGETA_S3_NATIVE_ACCESSES:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/native-accesses.jsonl}"
NATIVE_PLAN="${VEGETA_S3_NATIVE_PLAN:-benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl}"
INSTANCE_CATALOG="${VEGETA_S3_NATIVE_INSTANCE_CATALOG:-benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-instance-catalog.json}"
TRANSLATION_COVERAGE="${VEGETA_S3_TRANSLATION_COVERAGE:-benchmarks/corpora/vegeta-ethereum/s3/native-plan/translation-coverage.json}"
FINAL_MAPPING_SIMULATION="${VEGETA_S3_FINAL_MAPPING_SIMULATION:-benchmarks/corpora/vegeta-ethereum/s3/native-plan/final-mapping-simulation.json}"
GATES="${VEGETA_S3_EXACT_FOLLOWUP_GATES:-evaluation/vegeta/s3-exact-followup-gates.v1.json}"
OUT="${VEGETA_S3_EXACT_FOLLOWUP_OUT:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/exact-followup}"
TOP="${VEGETA_S3_EXACT_FOLLOWUP_TOP:-25}"

ARGS=(
  --exact-corpus "$EXACT_CORPUS"
  --native-accesses "$NATIVE_ACCESSES"
  --native-plan "$NATIVE_PLAN"
  --instance-catalog "$INSTANCE_CATALOG"
  --translation-coverage "$TRANSLATION_COVERAGE"
  --final-mapping-simulation "$FINAL_MAPPING_SIMULATION"
  --gates "$GATES"
  --output-dir "$OUT"
  --top "$TOP"
  --strict-gates
)

if [[ -f "$PUBLIC_CORPUS" ]]; then
  ARGS+=(--public-corpus "$PUBLIC_CORPUS")
else
  echo "warning: public source corpus not found; skipping exact-vs-prestate ablation: $PUBLIC_CORPUS" >&2
  ARGS+=(--no-public-ablation)
fi

python3 tools/vegeta/evaluate-vegeta-s3-exact-followup.py "${ARGS[@]}"

printf '\nVegeta S3 exact follow-up outputs:\n'
printf '  %s/exact-fidelity-followup.txt\n' "$OUT"
printf '  %s/exact-fidelity-followup.json\n' "$OUT"
printf '  %s/exact-fidelity-fn-ranking.csv\n' "$OUT"
printf '  %s/exact-fidelity-hot-keys.csv\n' "$OUT"
printf '  %s/exact-fidelity-critical-blocks.csv\n' "$OUT"
printf '  %s/exact-fidelity-mapping-per-block.csv\n' "$OUT"
