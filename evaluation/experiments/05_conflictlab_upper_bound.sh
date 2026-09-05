#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"; source "$ROOT/evaluation/lib/common.sh"
cd "$ROOT"
TX="${PAPER_EVAL_UB_TX:-384}"; BLOCKS="${PAPER_EVAL_UB_BLOCKS:-1}"; WORK="${PAPER_EVAL_UB_WORK:-786432}"
DIR="$RESULT_ROOT/05-upper-bound"; INPUT="$DIR/inputs"
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
rm -rf "$DIR"; mkdir -p "$INPUT"
python3 evaluation/workloads/generate_conflictlab.py --output-dir "$INPUT" --blocks "$BLOCKS" --transactions "$TX" --lanes "$TX" --work-iterations "$WORK" --tag upper-bound
bash "$ROOT/evaluation/lib/run_wasmd_dataset.sh" --name conflictlab-zero-conflict --execution-dir "$INPUT" --symbolic-dir "$INPUT/symbolic" --output "$DIR" --workers "$WORKERS" --samples "$SAMPLES" --blocks "$BLOCKS" --compute-scale 0
HOST=native-linux; grep -qiE 'microsoft|wsl' /proc/version /proc/sys/kernel/osrelease 2>/dev/null && HOST=wsl || true
python3 evaluation/wasmd/summarize_conflictlab_upper_bound.py --records "$DIR/records.jsonl" --output "$DIR/upper-bound-report.txt" --json-output "$DIR/upper-bound-report.json" --host-kind "$HOST" --max-reexec-pct "${PAPER_EVAL_UB_MAX_REEXEC_PCT:-0.5}"
