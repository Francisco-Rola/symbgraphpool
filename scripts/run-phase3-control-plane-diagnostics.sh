#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

echo '=== baseline v3 diagnostics ==='
"$ROOT/scripts/run-control-plane-corrections-diagnostics.sh"

echo '=== Phase 3 final ordering-DAG compression / exploration ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  dense_soft_ordering_is_transitively_reduced_after_wave_placement -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  deterministic_exploration_can_raise_the_effective_soft_risk_budget -- --nocapture

echo '=== Phase 3 runtime feedback / serial bypass ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --all-targets -- --nocapture

echo '=== Phase 3 bucketed ConflictLab prediction ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  conflictlab_prediction_quality_modes_expose_soft_edges_and_runtime_misses -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  serial_bypass_skips_candidate_graph_after_losing_warmup_economics -- --nocapture

echo '=== Phase 3 matrix contract ==='
python3 - "$ROOT" <<'PY'
import json
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
def expand(relative):
    grid = root / relative
    with tempfile.TemporaryDirectory() as temp:
        manifest_path = Path(temp) / "manifest.json"
        subprocess.run(
            [sys.executable, str(root / "scripts/generate-manifest-matrix.py"), str(grid), str(manifest_path)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        return json.loads(manifest_path.read_text(encoding="utf-8"))

manifest = expand("evaluation/conflictlab/phase3-system.grid.json")
runs = manifest["runs"]
if len(runs) != 864:
    raise SystemExit(f"expected 864 Phase 3 system runs, got {len(runs)}")
if {int(r["parameters"]["sim.block_size"]) for r in runs} != {32, 128, 512}:
    raise SystemExit("Phase 3 block-size axis changed")
if {r["parameters"]["complexity"] for r in runs} != {"light", "medium", "heavy"}:
    raise SystemExit("Phase 3 must keep three real-Wasm complexity levels")
if {r["parameters"]["prediction_quality"] for r in runs} != {"exact", "bucketed"}:
    raise SystemExit("Phase 3 prediction axis must be exact + bucketed")
if {r["parameters"]["acg.serial_bypass_enabled"] for r in runs} != {"false", "true"}:
    raise SystemExit("Phase 3 serial-bypass axis changed")
if {r["parameters"]["acg.risk_budget"] for r in runs} != {"0.50", "0.90"}:
    raise SystemExit("Phase 3 risk-budget axis changed")

exploration = expand("evaluation/conflictlab/phase3-exploration.grid.json")
exploration_runs = exploration["runs"]
if len(exploration_runs) != 108:
    raise SystemExit(f"expected 108 Phase 3 exploration runs, got {len(exploration_runs)}")
if {r["parameters"]["acg.exploration_rate"] for r in exploration_runs} != {"0.00", "0.05", "0.15"}:
    raise SystemExit("Phase 3 exploration-rate axis changed")
if {r["parameters"]["complexity"] for r in exploration_runs} != {"light", "medium", "heavy"}:
    raise SystemExit("Phase 3 exploration study must retain transaction complexity")

all_runs = runs + exploration_runs
if any(r["parameters"].get("execution_backend") != "wasm" for r in all_runs):
    raise SystemExit("Phase 3 must use the real Wasm backend")
if any(r["workers"] > 6 for r in all_runs):
    raise SystemExit("Phase 3 exceeds the six-core evaluation ceiling")
if manifest.get("record_schema_version") != 3 or exploration.get("record_schema_version") != 3:
    raise SystemExit("Phase 3 requires ExperimentRecord schema v3")
print("Phase 3 matrices PASS: 864 system + 108 exploration runs; complexity retained throughout")
PY

echo '=== Phase 3 Python tooling ==='
python3 -m py_compile \
  "$ROOT/scripts/summarize-conflictlab-phase3.py" \
  "$ROOT/scripts/generate-manifest-matrix.py" \
  "$ROOT/scripts/aggregate-experiment.py"
"$ROOT/scripts/run-evaluation-tools-tests.sh"

echo 'PASS: Phase 3 control-plane diagnostics'
