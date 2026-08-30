#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
PUBLICATION_MODE=false
STRICT_PAPER_METRICS=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --publication) PUBLICATION_MODE=true; shift ;;
    --strict-paper-metrics) STRICT_PAPER_METRICS=true; shift ;;
    *) break ;;
  esac
done
if [[ $# -gt 1 ]]; then
  echo "usage: $0 [--publication] [--strict-paper-metrics] [output-directory]" >&2
  exit 2
fi
OUT="${1:-$ROOT/benchmark-results/vegeta-s3/$STAMP}"
GRID="$ROOT/evaluation/vegeta/s3-seven-strategy-smoke.grid.json"
CORPUS="$ROOT/benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl"

if [[ ! -s "$CORPUS" ]]; then
  echo "Vegeta S3 corpus missing: $CORPUS" >&2
  echo "reconstruct it first, e.g.:" >&2
  echo "  hosted/reproducible fallback:" >&2
  echo "    ETH_RPC_URL=https://YOUR_ETHEREUM_RPC python3 tools/vegeta/extract-vegeta-ethereum.py --trace-mode public-rpc --output-dir benchmarks/corpora/vegeta-ethereum/s3 --resume" >&2
  echo "  exact SLOAD/SSTORE publication ground truth (transaction-level, resumable):" >&2
  echo "    ETH_RPC_URL=https://YOUR_ETHEREUM_RPC bash tools/legacy-scripts/run-vegeta-s3-exact-trace.sh" >&2
  exit 2
fi

GIT_REVISION="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || true)"
GIT_STATUS_COUNT="$(git -C "$ROOT" status --porcelain=v1 2>/dev/null | wc -l | tr -d ' ')"
if [[ "$PUBLICATION_MODE" == true && "$GIT_STATUS_COUNT" != "0" ]]; then
  echo "publication Vegeta run refused: git tree is dirty ($GIT_STATUS_COUNT status entries)" >&2
  exit 2
fi

mkdir -p "$OUT"
VALIDATE_ARGS=("$CORPUS" --json-output "$OUT/validation-report.json")
if [[ "$STRICT_PAPER_METRICS" == true ]]; then
  VALIDATE_ARGS+=(--require-paper-chain-match --require-weth-hotspot)
fi
python3 "$ROOT/tools/vegeta/validate-vegeta-corpus.py" "${VALIDATE_ARGS[@]}"

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"
export ACG_BUILD_PROFILE=release
export ACG_VEGETA_TRACE_WASM="${ACG_VEGETA_TRACE_WASM:-$ROOT/benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_vegeta_trace.wasm}"

cat > "$OUT/environment.txt" <<EOF2
started_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
git_revision=$GIT_REVISION
git_status=$GIT_STATUS_COUNT
publication_mode=$PUBLICATION_MODE
strict_paper_metrics=$STRICT_PAPER_METRICS
workers=6
workload=Vegeta NSDI'25 S3 Ethereum trace port
corpus=$CORPUS
EOF2

echo '=== build Vegeta trace-replay CosmWasm ==='
cargo build \
  --manifest-path "$ROOT/benchmarks/Cargo.toml" \
  -p acg-benchmark-vegeta-trace \
  --release \
  --target wasm32-unknown-unknown

MANIFEST="$OUT/manifest.json"
RECORDS="$OUT/records.jsonl"
ACCEPTANCE="$OUT/acceptance.json"
python3 "$ROOT/tools/internal/generate-manifest-matrix.py" "$GRID" "$MANIFEST"

cargo run --release \
  --manifest-path "$ROOT/runtime/Cargo.toml" \
  -p acg-benchmark-harness \
  --bin acg-benchmark \
  -- \
  "$MANIFEST" "$RECORDS" "$ACCEPTANCE" "$ROOT"

python3 "$ROOT/tools/internal/aggregate-experiment.py" "$RECORDS" --out-dir "$OUT/aggregate"
python3 "$ROOT/tools/internal/summarize-vegeta-s3.py" \
  "$RECORDS" \
  --output "$OUT/vegeta-s3-report.txt" \
  --csv-output "$OUT/vegeta-s3-matched-serial.csv"

echo "PASS: Vegeta S3 seven-strategy smoke completed"
echo "artifacts: $OUT"
