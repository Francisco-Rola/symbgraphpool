#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# Local development profile: on a 6-physical-core host this resolves to 2,4,6
# workers with one sample. Override EVAL_WASMD_WORKERS/EVAL_WASMD_SAMPLES freely.
exec bash scripts/eval-wasmd.sh debug
