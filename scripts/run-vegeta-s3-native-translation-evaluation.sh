#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
CHAR_DIR="${VEGETA_S3_CHARACTERIZATION:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"
OUTPUT_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
FAMILY_MAP="${VEGETA_S3_NATIVE_FAMILY_MAP:-evaluation/vegeta/s3-native-family-map.v1.json}"
GATE_CONFIG="${VEGETA_S3_NATIVE_GATE_CONFIG:-evaluation/vegeta/s3-native-preexecution-gates.v1.json}"

python3 scripts/vegeta/build-native-s3-plan.py \
  --corpus "$CORPUS" \
  --characterization-dir "$CHAR_DIR" \
  --family-map "$FAMILY_MAP" \
  --output-dir "$OUTPUT_DIR"

GAP_ARGS=()
if [[ "${VEGETA_S3_BACKGROUND_FETCH_SOURCE:-0}" == "1" ]]; then
  GAP_ARGS+=(--fetch-source)
fi
if [[ -n "${VEGETA_S3_BACKGROUND_SOURCE_LIMIT:-}" ]]; then
  GAP_ARGS+=(--source-limit "$VEGETA_S3_BACKGROUND_SOURCE_LIMIT")
fi
python3 scripts/vegeta/build-native-background-gap.py \
  --plan-dir "$OUTPUT_DIR" \
  --characterization-dir "$CHAR_DIR" \
  "${GAP_ARGS[@]}"

FINAL_ARGS=()
if [[ "${VEGETA_S3_FINAL_FETCH_PROXY:-0}" == "1" ]]; then
  FINAL_ARGS+=(--fetch-proxies)
fi
if [[ "${VEGETA_S3_FINAL_FETCH_SOURCE:-${VEGETA_S3_BACKGROUND_FETCH_SOURCE:-0}}" == "1" ]]; then
  FINAL_ARGS+=(--fetch-source)
fi
if [[ -n "${VEGETA_S3_FINAL_FAMILY_LIMIT:-}" ]]; then
  FINAL_ARGS+=(--family-limit "$VEGETA_S3_FINAL_FAMILY_LIMIT")
fi
if [[ -n "${VEGETA_S3_FINAL_PROXY_FAMILY_LIMIT:-}" ]]; then
  FINAL_ARGS+=(--proxy-family-limit "$VEGETA_S3_FINAL_PROXY_FAMILY_LIMIT")
fi
python3 scripts/vegeta/finalize-native-s3-map.py \
  --plan-dir "$OUTPUT_DIR" \
  --characterization-dir "$CHAR_DIR" \
  --gate-config "$GATE_CONFIG" \
  --base-family-map "$FAMILY_MAP" \
  "${FINAL_ARGS[@]}"

# The validator now consumes the selector-granular simulation for the frozen semantic-volume gates.
# If the compact final mapping still misses a frozen threshold, validation intentionally exits 2
# after all diagnostic artifacts have already been written.
python3 scripts/vegeta/validate-native-s3-plan.py \
  --corpus "$CORPUS" \
  --plan-dir "$OUTPUT_DIR" \
  --family-map "$FAMILY_MAP" \
  --gate-config "$GATE_CONFIG" \
  --simulation "$OUTPUT_DIR/final-mapping-simulation.json"

printf '\nTranslation/finalization artifacts:\n'
printf '  %s/native-plan.jsonl\n' "$OUTPUT_DIR"
printf '  %s/translation-coverage.txt\n' "$OUTPUT_DIR"
printf '  %s/background-gap-dossier.txt\n' "$OUTPUT_DIR"
printf '  %s/final-native-family-map.v2.json\n' "$OUTPUT_DIR"
printf '  %s/selector-semantic-map.json\n' "$OUTPUT_DIR"
printf '  %s/background-proxy-resolution.json\n' "$OUTPUT_DIR"
printf '  %s/background-rank1-diagnostic.txt\n' "$OUTPUT_DIR"
printf '  %s/final-mapping-simulation.txt\n' "$OUTPUT_DIR"
printf '  %s/preexecution-gate-report.txt\n' "$OUTPUT_DIR"
printf '  %s/validation-report.txt\n' "$OUTPUT_DIR"
printf '\nFrozen fidelity gates: aggregate conflict >=95%%, median conflict-bearing-block >=80%%, semantic tx >=75%%, semantic call frames >=50%%.\n'
printf 'To structurally resolve high-impact background proxies and source-resolve new implementations, run:\n'
printf '  ETH_RPC_URL=<archive-rpc> VEGETA_S3_BACKGROUND_FETCH_SOURCE=1 VEGETA_S3_FINAL_FETCH_PROXY=1 bash scripts/run-vegeta-s3-native-translation-evaluation.sh\n'
printf '\nTo require all seven base native contracts and genuine symbolic analyses later, run:\n'
printf '  python3 scripts/vegeta/validate-native-s3-plan.py --corpus %q --plan-dir %q --family-map %q --gate-config %q --simulation %q --require-execution-ready\n' "$CORPUS" "$OUTPUT_DIR" "$FAMILY_MAP" "$GATE_CONFIG" "$OUTPUT_DIR/final-mapping-simulation.json"
