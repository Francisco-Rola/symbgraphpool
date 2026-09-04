#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT_DIR="${S1_WASMD_FULL_W2_OUTPUT_DIR:-benchmark-results/wasmd-s1-full-w2}"
GO_TOOLCHAIN="${S1_WASMD_GO_TOOLCHAIN:-auto}"
ITER_PER_NS="${S1_WASMD_GO_ITERATIONS_PER_NANO:-0.54356250900000003}"

# This is intentionally one fixed comparison configuration. Do not inherit
# individual-debug-run switches that can make strategy artifacts incomparable.
unset EVAL_WASMD_ONLY_STRATEGY
unset EVAL_WASMD_CAMPAIGN_PROFILE_DIR
unset VEGETA_S3_WASMD_INVESTIGATE
export VEGETA_S3_RUST_ACG_ONLY=0
export VEGETA_S3_RUST_ACG_EDGE_MATERIALIZATION_THRESHOLD=""
export VEGETA_S3_RUST_ACG_SOFT_THRESHOLD=""
export VEGETA_S3_RUST_ACG_HARD_THRESHOLD=""
export VEGETA_S3_RUST_ACG_RISK_BUDGET=""
export VEGETA_S3_RUST_ACG_EXPLORATION_RATE=""
export VEGETA_S3_RUST_ACG_EXPLORATION_RISK_BUDGET=""
export VEGETA_S3_RUST_ACG_EXPLORATION_MIN_UNCERTAINTY=""
export VEGETA_S3_RUST_ACG_EXPLORATION_MAX_TRANSACTIONS=""
export VEGETA_S3_RUST_ACG_INDEPENDENCE_BEFORE_SOFTENING=""
export VEGETA_S3_RUST_ACG_SOFTENING_MIN_CONFIDENCE=""

export EVAL_WASMD_WORKERS=2
export EVAL_WASMD_SAMPLES=1
export EVAL_WASMD_MAX_BLOCKS=5000
export EVAL_WASMD_COMPUTE_SCALE=4
export EVAL_WASMD_COMPUTE_BASE_TOTAL_MS=1000
export EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS"
export EVAL_WASMD_IAVL_CACHE_SIZE=0
export EVAL_WASMD_IAVL_SYNC_PRUNING=1
export EVAL_WASMD_ISOLATE_STRATEGIES=1
export EVAL_WASMD_REUSE_SETUP_TEMPLATE=1
export EVAL_WASMD_SETUP_CHECK=0
export EVAL_WASMD_EXACT_ORACLE=0
export EVAL_WASMD_STREAM_PLAN=1
export EVAL_WASMD_ALLOWED_MISSING_SOURCE=0
export EVAL_WASMD_OUTPUT_DIR="$OUT_DIR"
export EVAL_WASMD_OVERWRITE=1
export EVAL_WASMD_REUSE_ISOLATED_PARTS=1
export EVAL_WASMD_REUSE_WEIGHTS=1
# Build the evaluator once before the isolated campaign. All strategy subprocesses use
# that exact output binary, keeping the comparison on one evaluator SHA.
export EVAL_WASMD_BUILD=1
export EVAL_WASMD_GO_TOOLCHAIN="$GO_TOOLCHAIN"
export EVAL_WASMD_REQUIRE_CLEAN=0

cat <<EOF
S1 Wasmd unified full-domain campaign
  blocks:             5000
  workers:            2
  samples:            1
  strategies:         serial, blockstm, ariafb, symbgraph-rust, vegeta
  exact oracle:       disabled
  compute scale:      4x gas_used
  go iter/ns:         $ITER_PER_NS
  IAVL cache:         0
  sync pruning:       true
  isolated processes: true
  output:             $OUT_DIR
  resume parts:       enabled (strict metadata + Serial-oracle hash checks)
EOF

if [[ "${S1_WASMD_SKIP_PREFLIGHT:-0}" != "1" ]]; then
  echo
  echo "=== preflight: Go tests ==="
  (
    cd benchmarks/cosmos-wasmd-blockstm-s3
    GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test ./...
  )
  echo
  echo "=== preflight: Wasmd publication-script regression tests ==="
  python3 -m unittest tools.tests.test_wasmd_publication_eval
fi

echo
echo "=== unified S1 Wasmd campaign ==="
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh debug

echo
echo "=== fail-closed campaign verification ==="
python3 evaluation/wasmd/verify_s1_full_w2.py \
  --output-dir "$OUT_DIR" \
  --blocks 5000 \
  --workers 2 \
  --samples 1 \
  --dataset vegeta-s1-wasmd \
  --compute-scale 4 \
  --iavl-cache-size 0 \
  --iavl-sync-pruning 1
