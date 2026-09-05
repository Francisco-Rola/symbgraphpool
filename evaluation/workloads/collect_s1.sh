#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
: "${ETH_RPC_URL:?set ETH_RPC_URL to an Ethereum archive/debug RPC}"
bash tools/legacy-scripts/run-vegeta-s1-collect.sh
