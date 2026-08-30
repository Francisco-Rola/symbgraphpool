#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

EXACT_CORPUS="${VEGETA_S3_EXACT_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/corpus.jsonl}"
FAMILY_MAP="${VEGETA_S3_NATIVE_FAMILY_MAP:-evaluation/vegeta/s3-native-family-map.v2.json}"
PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"

if [[ ! -s "$EXACT_CORPUS" ]]; then
  echo "missing exact SLOAD/SSTORE corpus: $EXACT_CORPUS" >&2
  exit 2
fi
if [[ ! -s "$FAMILY_MAP" ]]; then
  echo "missing v2 native family map: $FAMILY_MAP" >&2
  exit 2
fi

# Reuse the completed exact trace.  The exact corpus is now also the authoritative source for the
# frozen conflict-coverage gates; call/calldata planning still never imports its concrete R/W keys.
export VEGETA_S3_CORPUS="$EXACT_CORPUS"
export VEGETA_S3_NATIVE_FAMILY_MAP="$FAMILY_MAP"
export VEGETA_S3_NATIVE_PLAN_DIR="$PLAN_DIR"
export VEGETA_S3_NATIVE_EXECUTION_DIR="$EXEC_DIR"

bash tools/legacy-scripts/validate-vegeta-s3-exact-family-extensions.sh
bash tools/legacy-scripts/run-vegeta-s3-native-translation-evaluation.sh
bash tools/legacy-scripts/run-vegeta-s3-native-execution.sh
bash tools/legacy-scripts/run-vegeta-s3-exact-followup.sh
python3 tools/vegeta/validate-vegeta-s3-exact-family-extension-results.py \
  --followup "$EXEC_DIR/exact-followup/exact-fidelity-followup.json" \
  --instance-catalog "$PLAN_DIR/native-instance-catalog.json" \
  --output-dir "$EXEC_DIR/exact-family-extension-validation"

printf '\nExact family-extension outputs:\n'
printf '  %s/final-mapping-simulation.txt\n' "$PLAN_DIR"
printf '  %s/native-topology-fidelity.txt\n' "$EXEC_DIR"
printf '  %s/exact-followup/exact-fidelity-followup.txt\n' "$EXEC_DIR"
printf '  %s/exact-family-extension-validation/validation.txt\n' "$EXEC_DIR"
printf '\nNo exact trace extraction is rerun by this script.\n'
