#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export VEGETA_S1_READINESS_PROFILE="scheduler-fidelity"
exec bash "$ROOT/tools/legacy-scripts/run-vegeta-s1-prepare-native.sh" "$@"
