#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-benchmark-results/conflictlab-v1-core}"
DEST="${2:-$OUT/debug-bundles}"

if [[ ! -d "$OUT" ]]; then
  echo "ConflictLab V1 output directory does not exist: $OUT" >&2
  exit 2
fi
if [[ ! -s "$OUT/records.jsonl" ]]; then
  echo "ConflictLab V1 combined records are missing or empty: $OUT/records.jsonl" >&2
  exit 2
fi

mkdir -p "$DEST"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
BUNDLE="$DEST/conflictlab-v1-debug-$STAMP.zip"

python3 - "$ROOT" "$OUT" "$BUNDLE" <<'PY'
from __future__ import annotations

import json
import os
import platform
import subprocess
import sys
import zipfile
from datetime import datetime, timezone
from pathlib import Path

root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
bundle = Path(sys.argv[3]).resolve()
bundle.parent.mkdir(parents=True, exist_ok=True)

archived: list[dict[str, object]] = []


def add(path: Path, arcname: str) -> None:
    if not path.is_file():
        return
    # Do not recursively archive a prior debug bundle.
    try:
        if bundle.parent in path.parents:
            return
    except RuntimeError:
        pass
    zf.write(path, arcname)
    archived.append({"path": arcname, "bytes": path.stat().st_size})


def capture(args: list[str]) -> str:
    try:
        result = subprocess.run(
            args,
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=15,
            check=False,
        )
        return result.stdout.strip()
    except Exception as exc:  # diagnostics must never hide the primary failure
        return f"<capture failed: {exc}>"


with zipfile.ZipFile(bundle, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as zf:
    # The combined records plus post-processing outputs are the fastest way to reproduce validator
    # failures without asking for a new experiment run.
    for name in (
        "records.jsonl",
        "validation.txt",
        "results-summary.txt",
        "paper-analysis.md",
        "summary-run.log",
        "campaign-counts.json",
        "campaign-status.csv",
        "suite-environment.txt",
    ):
        add(out / name, f"results/{name}")

    aggregate = out / "aggregate"
    if aggregate.is_dir():
        for path in sorted(aggregate.rglob("*")):
            if path.is_file() and path.suffix.lower() in {".csv", ".json", ".txt", ".md"}:
                add(path, f"results/aggregate/{path.relative_to(aggregate).as_posix()}")

    # Per-campaign manifests/acceptance/aggregate summaries identify exactly which cached data was
    # used while avoiding a second copy of every campaign records.jsonl.
    for campaign in sorted(p for p in out.iterdir() if p.is_dir() and p.name != "debug-bundles"):
        if not (campaign / "records.jsonl").exists():
            continue
        for name in ("manifest.json", "acceptance.json", "environment.txt"):
            add(campaign / name, f"campaigns/{campaign.name}/{name}")
        campaign_aggregate = campaign / "aggregate"
        if campaign_aggregate.is_dir():
            for path in sorted(campaign_aggregate.rglob("*")):
                if path.is_file() and path.suffix.lower() in {".csv", ".json", ".txt", ".md"}:
                    add(
                        path,
                        f"campaigns/{campaign.name}/aggregate/{path.relative_to(campaign_aggregate).as_posix()}",
                    )

    # Include small diagnostic artifacts from prior targeted verifiers. Large state snapshots and
    # duplicate records are intentionally skipped; they can be supplied separately if needed.
    diagnostics = out / "correctness-diagnostics"
    if diagnostics.is_dir():
        for path in sorted(diagnostics.rglob("*")):
            if not path.is_file():
                continue
            if path.name == "records.jsonl" or path.stat().st_size > 5 * 1024 * 1024:
                continue
            if path.suffix.lower() not in {".csv", ".json", ".txt", ".log", ".md"}:
                continue
            add(path, f"diagnostics/{path.relative_to(diagnostics).as_posix()}")

    # Snapshot the evaluation policy and the runtime files most often involved in ConflictLab V1
    # debugging. This makes the bundle self-describing even after the working tree moves on.
    source_files = [
        "scripts/validate-conflictlab-v1.py",
        "scripts/conflictlab_v1_miss_policy.py",
        "scripts/run-conflictlab-v1-evaluation.sh",
        "scripts/summarize-conflictlab-v1.py",
        "scripts/check-conflictlab-v1-campaign-cache.py",
        "scripts/collect-conflictlab-v1-debug-bundle.sh",
        "runtime/crates/acg-benchmark-harness/src/conflictlab.rs",
        "runtime/crates/acg-runtime-feedback/src/lib.rs",
        "runtime/crates/acg-runtime-feedback/src/adaptive_pipeline.rs",
        "runtime/crates/acg-evaluation/src/lib.rs",
    ]
    for rel in source_files:
        add(root / rel, f"source/{rel}")
    for path in sorted((root / "evaluation/conflictlab").glob("v1-*.grid.json")):
        add(path, f"source/{path.relative_to(root).as_posix()}")
    add(root / "evaluation/conflictlab/v1-experimental-suite.md", "source/evaluation/conflictlab/v1-experimental-suite.md")

    debug_info = {
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "output_directory": str(out),
        "python": sys.version,
        "platform": platform.platform(),
        "git_head": capture(["git", "rev-parse", "HEAD"]),
        "git_status_short": capture(["git", "status", "--short"]),
        "git_diff_stat": capture(["git", "diff", "--stat"]),
        "git_last_commit": capture(["git", "log", "-1", "--oneline", "--decorate"]),
    }
    zf.writestr("metadata/debug-info.json", json.dumps(debug_info, indent=2) + "\n")
    archived.append({"path": "metadata/debug-info.json", "bytes": len(json.dumps(debug_info))})

    manifest = {
        "format": "conflictlab-v1-debug-bundle-v1",
        "files": archived,
    }
    zf.writestr("metadata/archive-manifest.json", json.dumps(manifest, indent=2) + "\n")

print(bundle)
PY
