#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# S3 is the 101-block exact/mechanism dataset. The native execution builder
# includes its implementation validation gate.
bash tools/legacy-scripts/run-vegeta-s3-native-execution.sh
