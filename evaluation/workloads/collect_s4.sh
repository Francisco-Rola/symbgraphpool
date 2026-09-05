#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
: "${ETH_RPC_URL:?set ETH_RPC_URL to an Ethereum archive/debug RPC}"
# Public-RPC prestate/diff + callTracer/code metadata only. Intentionally no
# per-transaction exact SLOAD/SSTORE pass, matching the scalable S1 path.
bash tools/legacy-scripts/run-vegeta-s4-collect-all.sh
