#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "NOTE: rerun-conflictlab-v1-safe-vm-suite.sh is retained only as a compatibility alias." >&2
echo "Canonical ConflictLab V1 now uses benchmark-scoped retained VM reuse with a non-binding cumulative gas budget," >&2
echo "and validates it against fresh/recycle state on the previously failing stress identities." >&2
echo "Delegating to rerun-conflictlab-v1-retained-vm-suite.sh." >&2
exec "$ROOT/scripts/rerun-conflictlab-v1-retained-vm-suite.sh" "$@"
