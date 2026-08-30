#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# Publication profile: physical-core scaling (powers of two plus all physical
# cores), five independent samples, and a clean committed tree by default.
exec bash scripts/eval-wasmd.sh paper
