#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
bash tools/legacy-scripts/run-vegeta-s1-prepare-native.sh
