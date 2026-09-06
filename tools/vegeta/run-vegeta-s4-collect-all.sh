#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

bash tools/vegeta/run-vegeta-s4-collect.sh

if [[ "${VEGETA_S4_SKIP_NATIVE_INPUTS:-0}" == "1" ]]; then
  echo
  echo "S4 corpus complete; VEGETA_S4_SKIP_NATIVE_INPUTS=1, skipping callTracer/code collection."
  exit 0
fi

bash tools/vegeta/run-vegeta-s4-native-inputs.sh
