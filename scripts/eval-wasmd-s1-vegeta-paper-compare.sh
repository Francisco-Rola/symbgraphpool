#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BLOCKS="${S1_VEGETA_COMPARE_BLOCKS:-300}"
OUT_DIR="${S1_VEGETA_COMPARE_OUTPUT_DIR:-benchmark-results/wasmd-s1-vegeta-paper-w2-b${BLOCKS}}"
ITER_PER_NS="${S1_VEGETA_COMPARE_ITER_PER_NS:-0.54356250900000003}"
GO_TOOLCHAIN="${S1_VEGETA_COMPARE_GO_TOOLCHAIN:-auto}"
[[ "$BLOCKS" =~ ^[1-9][0-9]*$ ]] || { echo "S1_VEGETA_COMPARE_BLOCKS must be positive" >&2; exit 2; }

unset EVAL_WASMD_CAMPAIGN_PROFILE_DIR
export EVAL_WASMD_WORKERS=2
export EVAL_WASMD_SAMPLES=1
export EVAL_WASMD_MAX_BLOCKS="$BLOCKS"
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
export EVAL_WASMD_REQUIRE_CLEAN=0
export EVAL_WASMD_GO_TOOLCHAIN="$GO_TOOLCHAIN"

cat <<EOF
S1 faithful-Vegeta paper-method comparison
  blocks:        $BLOCKS
  workers:       2
  samples:       1
  systems:       Serial, Vegeta
  compute:       gas_used x4
  iter/ns:       $ITER_PER_NS
  IAVL:          cache=0, sync pruning=true
  primary metric: tx / replay(post-consensus) time
  output:        $OUT_DIR
EOF

if [[ "${S1_VEGETA_COMPARE_SKIP_TESTS:-0}" != "1" ]]; then
  echo
  echo "=== preflight: Vegeta fidelity/unit tests ==="
  (
    cd benchmarks/cosmos-wasmd-blockstm-s3
    GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test ./...
  )
  python3 -m unittest tools.tests.test_wasmd_publication_eval
fi

echo
echo "=== matched Serial ==="
export EVAL_WASMD_ONLY_STRATEGY=serial
# Build the evaluator exactly once in the fresh comparison output.  Do not
# depend on eval-wasmd.sh having the newer BUILD=0 auto-rebuild behavior.
export EVAL_WASMD_BUILD=1
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh debug

BIN="$OUT_DIR/bin/wasmd-scheduler-eval"
[[ -x "$BIN" ]] || { echo "Serial phase did not produce evaluator binary: $BIN" >&2; exit 3; }
SERIAL_BIN_SHA="$(sha256sum "$BIN" | awk '{print $1}')"
echo "comparison evaluator sha256=$SERIAL_BIN_SHA"

echo
echo "=== faithful Vegeta replay ==="
export EVAL_WASMD_ONLY_STRATEGY=vegeta
export EVAL_WASMD_BUILD=0
bash tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh debug
unset EVAL_WASMD_ONLY_STRATEGY

VEGETA_BIN_SHA="$(sha256sum "$BIN" | awk '{print $1}')"
[[ "$VEGETA_BIN_SHA" == "$SERIAL_BIN_SHA" ]] || {
  echo "evaluator binary changed between Serial and Vegeta: serial=$SERIAL_BIN_SHA vegeta=$VEGETA_BIN_SHA" >&2
  exit 3
}

SERIAL="$OUT_DIR/raw/records-w2-serial.jsonl"
VEGETA="$OUT_DIR/raw/records-w2-vegeta.jsonl"
[[ -s "$SERIAL" && -s "$VEGETA" ]] || { echo "missing Serial/Vegeta raw output" >&2; exit 3; }

echo
echo "=== Vegeta paper-method comparison ==="
python3 evaluation/wasmd/compare_vegeta_paper.py --serial "$SERIAL" --vegeta "$VEGETA" \
  | tee "$OUT_DIR/vegeta-paper-comparison.txt"

if [[ "$BLOCKS" != "5000" ]]; then
  echo
  echo "300-block fidelity/performance gate complete. If the split metrics look healthy, run full S1 with:"
  echo "  S1_VEGETA_COMPARE_BLOCKS=5000 time bash scripts/eval-wasmd-s1-vegeta-paper-compare.sh"
fi
