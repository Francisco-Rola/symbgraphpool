#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=common.sh
source "$ROOT/evaluation/lib/common.sh"

usage() {
  cat <<'USAGE'
Usage: run_wasmd_dataset.sh --name NAME --execution-dir DIR --symbolic-dir DIR --output DIR [options]
Options:
  --source-corpus FILE       Optional source corpus for source/translated diagnostics.
  --vegeta-tag TAG           Optional S1/S3/S4 source provenance tag.
  --workers LIST             Comma-separated workers (default from PAPER_EVAL_PROFILE).
  --samples N                Samples per worker (default from PAPER_EVAL_PROFILE).
  --blocks N                 Prefix blocks; 0 means entire execution plan.
  --compute-scale N          Evaluator synthetic compute scale (default 0).
  --iter-per-ns X            Fixed calibration; default auto-calibrate once.
  --exact-oracle 0|1         Enable exact oracle metadata (default 0).
  --allowed-missing-source N Allow N execution-plan tx without frozen exact source traces (default 0).
  --stream-plan auto|0|1     Stream large execution plans when supported (default auto).
USAGE
}

NAME=""; EXEC_DIR=""; SYMBOLIC_DIR=""; OUTPUT=""; SOURCE_CORPUS=""; VEGETA_TAG=""
WORKERS_ARG="$WORKERS"; SAMPLES_ARG="$SAMPLES"; BLOCKS=0; COMPUTE_SCALE=0; ITER_PER_NS=""; EXACT_ORACLE=0; ALLOWED_MISSING_SOURCE=0; STREAM_PLAN_MODE="${PAPER_EVAL_STREAM_PLAN:-auto}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --name) NAME="$2"; shift 2;;
    --execution-dir) EXEC_DIR="$2"; shift 2;;
    --symbolic-dir) SYMBOLIC_DIR="$2"; shift 2;;
    --output) OUTPUT="$2"; shift 2;;
    --source-corpus) SOURCE_CORPUS="$2"; shift 2;;
    --vegeta-tag) VEGETA_TAG="$2"; shift 2;;
    --workers) WORKERS_ARG="$2"; shift 2;;
    --samples) SAMPLES_ARG="$2"; shift 2;;
    --blocks) BLOCKS="$2"; shift 2;;
    --compute-scale) COMPUTE_SCALE="$2"; shift 2;;
    --iter-per-ns) ITER_PER_NS="$2"; shift 2;;
    --exact-oracle) EXACT_ORACLE="$2"; shift 2;;
    --allowed-missing-source) ALLOWED_MISSING_SOURCE="$2"; shift 2;;
    --stream-plan) STREAM_PLAN_MODE="$2"; shift 2;;
    -h|--help) usage; exit 0;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2;;
  esac
done
[[ -n "$NAME" && -n "$EXEC_DIR" && -n "$SYMBOLIC_DIR" && -n "$OUTPUT" ]] || { usage >&2; exit 2; }
require_dir "$EXEC_DIR"
require_dir "$SYMBOLIC_DIR"
mkdir -p "$OUTPUT"

case "${STREAM_PLAN_MODE,,}" in
  auto)
    MANIFEST="$EXEC_DIR/execution-manifest.json"
    [[ -s "$MANIFEST" ]] || { echo "missing execution manifest: $MANIFEST" >&2; exit 2; }
    STREAM_PLAN_MODE="$(python3 - "$MANIFEST" <<'PYSTREAM'
import json, sys
obj=json.load(open(sys.argv[1], encoding="utf-8"))
logical=obj.get("logical_addresses")
try:
    blocks=int(obj.get("blocks", 0) or 0)
except (TypeError, ValueError):
    blocks=0
print(1 if isinstance(logical, list) and len(logical) > 0 and blocks > 0 else 0)
PYSTREAM
)"
    ;;
  1|true|yes|on) STREAM_PLAN_MODE=1 ;;
  0|false|no|off) STREAM_PLAN_MODE=0 ;;
  *) echo "--stream-plan must be auto, 0, or 1" >&2; exit 2 ;;
esac

if [[ -z "$ITER_PER_NS" ]]; then
  BIN="$ROOT/benchmarks/cosmos-wasmd-blockstm-s3/wasmd-blockstm-s3"
  (cd "$ROOT/benchmarks/cosmos-wasmd-blockstm-s3" && GOTOOLCHAIN="${PAPER_EVAL_GO_TOOLCHAIN:-auto}" go build -o "$BIN" .)
  ITER_PER_NS="$($BIN --calibrate-only | tail -1)"
  [[ -n "$ITER_PER_NS" ]] || { echo "failed to calibrate evaluator" >&2; exit 2; }
fi

export EVAL_WASMD_WORKERS="$WORKERS_ARG"
export EVAL_WASMD_SAMPLES="$SAMPLES_ARG"
export EVAL_WASMD_OUTPUT_DIR="$OUTPUT"
export EVAL_WASMD_EXEC_DIR="$EXEC_DIR"
export EVAL_WASMD_SYMBOLIC_DIR="$SYMBOLIC_DIR"
export EVAL_WASMD_DATASET="$NAME"
export EVAL_WASMD_COMPUTE_SCALE="$COMPUTE_SCALE"
export EVAL_WASMD_GO_ITERATIONS_PER_NANO="$ITER_PER_NS"
export EVAL_WASMD_IAVL_CACHE_SIZE="${EVAL_WASMD_IAVL_CACHE_SIZE:-0}"
export EVAL_WASMD_IAVL_SYNC_PRUNING="${EVAL_WASMD_IAVL_SYNC_PRUNING:-1}"
export EVAL_WASMD_MAX_BLOCKS="$BLOCKS"
export EVAL_WASMD_EXACT_ORACLE="$EXACT_ORACLE"
export EVAL_WASMD_CONSENSUS_WINDOWS_MS="${PAPER_EVAL_CONSENSUS_WINDOWS_MS:-${PAPER_EVAL_CONSENSUS_WINDOW_MS:-$CONSENSUS_WINDOWS_MS}}"
export EVAL_WASMD_ISOLATE_STRATEGIES=1
export EVAL_WASMD_REUSE_SETUP=1
export EVAL_WASMD_REUSE_ISOLATED_PARTS="${EVAL_WASMD_REUSE_ISOLATED_PARTS:-0}"
export EVAL_WASMD_REUSE_WEIGHTS="${EVAL_WASMD_REUSE_WEIGHTS:-1}"
export EVAL_WASMD_RESOURCE_ACCOUNTING="${PAPER_EVAL_RESOURCE_ACCOUNTING:-${EVAL_WASMD_RESOURCE_ACCOUNTING:-0}}"
export EVAL_WASMD_OVERWRITE="${PAPER_EVAL_OVERWRITE:-1}"
export EVAL_WASMD_SETUP_CHECK="${PAPER_EVAL_SETUP_CHECK:-0}"
export EVAL_WASMD_STREAM_PLAN="$STREAM_PLAN_MODE"
export EVAL_WASMD_ALLOWED_MISSING_SOURCE="${PAPER_EVAL_ALLOWED_MISSING_SOURCE:-$ALLOWED_MISSING_SOURCE}"
if [[ "$PROFILE" == paper ]]; then
  export EVAL_WASMD_REQUIRE_CLEAN="${PAPER_EVAL_REQUIRE_CLEAN:-1}"
else
  export EVAL_WASMD_REQUIRE_CLEAN="${PAPER_EVAL_REQUIRE_CLEAN:-0}"
fi
if [[ -n "$SOURCE_CORPUS" ]]; then export EVAL_WASMD_SOURCE_CORPUS="$SOURCE_CORPUS"; fi
if [[ -n "$VEGETA_TAG" ]]; then export EVAL_WASMD_VEGETA_DATASET_TAG="$VEGETA_TAG"; fi

printf 'Wasmd dataset campaign: %s\n  workers=%s samples=%s blocks=%s compute-scale=%s iter/ns=%s stream-plan=%s\n' \
  "$NAME" "$WORKERS_ARG" "$SAMPLES_ARG" "$BLOCKS" "$COMPUTE_SCALE" "$ITER_PER_NS" "$STREAM_PLAN_MODE"
bash "$ROOT/evaluation/lib/run_wasmd_campaign.sh" paper
