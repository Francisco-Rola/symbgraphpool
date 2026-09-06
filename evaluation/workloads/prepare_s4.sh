#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
S4_DIR="${VEGETA_S4_DIR:-benchmarks/corpora/vegeta-ethereum/s4}"
CORPUS="${VEGETA_S4_CORPUS:-$S4_DIR/corpus.jsonl}"
FAMILY_MAP="${VEGETA_S4_NATIVE_FAMILY_MAP:-evaluation/vegeta/s4-native-family-map.v1.json}"
[[ -s "$CORPUS" ]] || { echo "S4 corpus not collected yet; run evaluation/workloads/collect_s4.sh" >&2; exit 2; }

if [[ ! -s "$FAMILY_MAP" ]]; then
  echo "No reviewed S4 family map is frozen yet; running local characterization first." >&2
  bash tools/vegeta/run-vegeta-s4-characterize.sh
  cat <<MSG >&2

S4 characterization is complete, but native preparation remains fail-closed.
Review: $S4_DIR/native-characterization/family-review-queue.md
Candidate: $S4_DIR/native-characterization/s4-native-family-map.candidate.json
Freeze a reviewed map as: $FAMILY_MAP
Then rerun: bash evaluation/workloads/prepare_s4.sh
MSG
  exit 3
fi

exec bash tools/vegeta/run-vegeta-s4-prepare-native.sh
