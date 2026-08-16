#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 <manifest.json> <records.jsonl> [acceptance-report.json]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

cargo run \
  --quiet \
  --manifest-path runtime/Cargo.toml \
  -p acg-evaluation \
  --bin acg-evaluate \
  -- "$@"
