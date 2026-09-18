#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT"

R="${PAPER_EVAL_RESULT_ROOT:-$ROOT/benchmark-results/eurosys/local-6c-paper}"
OUT="${1:-$ROOT/eurosys-local-6c-review.zip}"
STAGE="$(mktemp -d)"
DEST="$STAGE/eurosys-review"

cleanup() {
    rm -rf "$STAGE"
}
trap cleanup EXIT

mkdir -p "$DEST/results" "$DEST/code" "$DEST/meta"

copy_file() {
    local src="$1"
    local dst="$2"
    [[ -f "$src" ]] || return 0
    mkdir -p "$(dirname "$dst")"
    cp -a "$src" "$dst"
}

copy_tree_filtered() {
    local src="$1"
    local dst="$2"
    [[ -d "$src" ]] || return 0

    mkdir -p "$dst"
    while IFS= read -r -d '' f; do
        local rel="${f#"$src"/}"
        mkdir -p "$dst/$(dirname "$rel")"
        cp -a "$f" "$dst/$rel"
    done < <(
        find "$src" -type f \
            \( -name '*.csv' \
            -o -name '*.json' \
            -o -name '*.jsonl' \
            -o -name '*.txt' \
            -o -name '*.pdf' \) \
            -print0
    )
}

copy_top_filtered() {
    local src="$1"
    local dst="$2"
    [[ -d "$src" ]] || return 0

    mkdir -p "$dst"
    while IFS= read -r -d '' f; do
        cp -a "$f" "$dst/"
    done < <(
        find "$src" -maxdepth 1 -type f \
            \( -name 'records.jsonl' \
            -o -name '*.csv' \
            -o -name '*.json' \
            -o -name '*.txt' \) \
            -print0
    )
}

echo "Building EuroSys review bundle"
echo "  repository: $ROOT"
echo "  results:    $R"
echo "  output:     $OUT"

if [[ ! -d "$R" ]]; then
    echo "error: result root does not exist: $R" >&2
    exit 1
fi

# Final publication artifacts and headline-derived summaries.
copy_tree_filtered "$R/paper" "$DEST/results/paper"
copy_tree_filtered "$R/eurosys-summary" "$DEST/results/eurosys-summary"

# Machine and campaign provenance.
copy_file "$R/machine.json" "$DEST/meta/machine.json"
copy_file "$R/.overnight-rest/summary.txt" "$DEST/meta/overnight-summary.txt"
copy_file "$R/.cluster-paper/summary.txt" "$DEST/meta/cluster-paper-summary.txt"
copy_file "$R/cluster-allocation.txt" "$DEST/meta/cluster-allocation.txt"
if [[ -d "$R/.overnight-rest" ]]; then
    mkdir -p "$DEST/meta/overnight-state"
    find "$R/.overnight-rest" -maxdepth 1 -type f \
        \( -name '*.done' -o -name '*.failed' -o -name '*.txt' \) \
        -exec cp -a {} "$DEST/meta/overnight-state/" \;
fi
if [[ -d "$R/.cluster-paper" ]]; then
    mkdir -p "$DEST/meta/cluster-paper-state"
    find "$R/.cluster-paper" -maxdepth 1 -type f \
        \( -name '*.done' -o -name '*.failed' -o -name '*.txt' -o -name '*.env' \) \
        -exec cp -a {} "$DEST/meta/cluster-paper-state/" \;
fi

# Headline S1/S4 and S3 phase/cost data. Keep raw records and nested summaries.
copy_tree_filtered "$R/01-s1" "$DEST/results/01-s1"
copy_tree_filtered "$R/02-s4" "$DEST/results/02-s4"
copy_tree_filtered "$R/03-s3-breakdown" "$DEST/results/03-s3-breakdown"

# Native workloads and contention sweeps.
copy_tree_filtered "$R/04-native" "$DEST/results/04-native"
copy_tree_filtered "$R/05-upper-bound" "$DEST/results/05-upper-bound"
copy_tree_filtered "$R/06-contention" "$DEST/results/06-contention"

# Prediction, recovery, adaptation and ACG ablations.
copy_tree_filtered "$R/07-prediction" "$DEST/results/07-prediction"
copy_tree_filtered "$R/08-adaptation" "$DEST/results/08-adaptation"
# The experiment writes 09-s3-ablation; retain the legacy spelling too if present.
copy_tree_filtered "$R/09-s3-ablation" "$DEST/results/09-s3-ablation"
copy_tree_filtered "$R/09-s3-acg-ablation" "$DEST/results/09-s3-acg-ablation"

# Block-size, candidate/final-order robustness, and semantic validation.
copy_tree_filtered "$R/10-block-size" "$DEST/results/10-block-size"
copy_tree_filtered "$R/11-consensus" "$DEST/results/11-consensus"
copy_tree_filtered "$R/12-semantics" "$DEST/results/12-semantics"
copy_tree_filtered "$R/13-compaction" "$DEST/results/13-compaction"
copy_tree_filtered "$R/14-consensus-window-sensitivity" \
    "$DEST/results/14-consensus-window-sensitivity"

# Sensitivity and translation-fidelity data.
copy_tree_filtered "$R/15-compute-sensitivity" \
    "$DEST/results/15-compute-sensitivity"
copy_tree_filtered "$R/16-translation-fidelity" \
    "$DEST/results/16-translation-fidelity"
copy_tree_filtered "$R/17-iavl-sensitivity" \
    "$DEST/results/17-iavl-sensitivity"

# Preserve the exact postprocessing / plotting code used to make the PDFs.
if [[ -d evaluation/eurosys ]]; then
    while IFS= read -r -d '' f; do
        rel="${f#"$ROOT"/}"
        mkdir -p "$DEST/code/$(dirname "$rel")"
        cp -a "$f" "$DEST/code/$rel"
    done < <(
        find "$ROOT/evaluation/eurosys" -type f \
            \( -name '*.py' -o -name '*.sh' \) \
            -print0
    )
fi

# Preserve experiment drivers so metric definitions can be audited.
if [[ -d evaluation/experiments ]]; then
    while IFS= read -r -d '' f; do
        rel="${f#"$ROOT"/}"
        mkdir -p "$DEST/code/$(dirname "$rel")"
        cp -a "$f" "$DEST/code/$rel"
    done < <(
        find "$ROOT/evaluation/experiments" -maxdepth 1 -type f \
            -name '*.sh' -print0
    )
fi

# Repository provenance. This intentionally records local plotting changes too.
git rev-parse HEAD > "$DEST/meta/git-head.txt" 2>/dev/null || true
git status --short > "$DEST/meta/git-status.txt" 2>/dev/null || true
git diff --stat > "$DEST/meta/git-diff-stat.txt" 2>/dev/null || true
git diff > "$DEST/meta/git-diff.patch" 2>/dev/null || true

{
    echo "created=$(date -Is)"
    echo "result_root=$R"
    echo
    echo '[uname]'
    uname -a || true
    echo
    echo '[go]'
    go version 2>/dev/null || true
    echo
    echo '[rust]'
    rustc --version 2>/dev/null || true
    cargo --version 2>/dev/null || true
    echo
    echo '[python]'
    python3 --version 2>/dev/null || true
} > "$DEST/meta/environment.txt"

(
    cd "$DEST"
    find . -type f -printf '%P\t%s bytes\n' | sort
) > "$DEST/meta/file-manifest.txt"

# Use Python's standard library so the helper does not depend on the zip CLI.
python3 - "$STAGE" "$OUT" <<'PY'
from pathlib import Path
import sys
import zipfile

stage = Path(sys.argv[1])
out = Path(sys.argv[2]).resolve()
root = stage / "eurosys-review"
out.parent.mkdir(parents=True, exist_ok=True)

with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as zf:
    for path in sorted(root.rglob("*")):
        if path.is_file():
            zf.write(path, path.relative_to(stage))
PY

echo
echo "Created review bundle:"
ls -lh "$OUT"
echo
echo "Upload this file for validation:"
echo "$OUT"
