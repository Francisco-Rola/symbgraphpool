#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

echo '=== Phase 3 regression diagnostics ==='
"$ROOT/scripts/run-phase3-control-plane-diagnostics.sh"

echo '=== Phase 4 compact candidate construction ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --test weighted_candidate_graph \
  immature_equivalence_clique_is_materialized_as_a_chain_with_logical_coverage -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  compact_equivalence_planning_keeps_logical_candidate_coverage -- --nocapture

echo '=== Phase 4 targeted exploration ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  deterministic_exploration_can_raise_the_effective_soft_risk_budget -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  targeted_exploration_skips_confident_soft_relationships -- --nocapture

echo '=== Phase 4 true serial fallback / complexity-aware admission ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  serial_bypass_skips_candidate_graph_after_losing_warmup_economics -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-evaluation --test acceptance \
  schema_v3_acceptance_understands_true_serial_bypass_execution -- --nocapture

echo '=== Phase 4 mixed-complexity generation ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  conflictlab_mixed_complexity_is_deterministic_and_reported -- --nocapture

echo '=== Phase 4 reusable Wasm instance semantics / cache lifetime ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-cosmwasm-engine --test wasm_smoke -- --nocapture

echo '=== Phase 4 matrix contract ==='
python3 - "$ROOT" <<'PY'
import json
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
checks = [
    ("evaluation/conflictlab/phase4-system.grid.json", 432),
    ("evaluation/conflictlab/phase4-vm-lifecycle.grid.json", 36),
    ("evaluation/conflictlab/phase4-mixed.grid.json", 216),
    ("evaluation/conflictlab/phase4-exploration.grid.json", 48),
]

def expand(relative):
    with tempfile.TemporaryDirectory() as temp:
        manifest_path = Path(temp) / "manifest.json"
        subprocess.run(
            [sys.executable, str(root / "scripts/generate-manifest-matrix.py"), str(root / relative), str(manifest_path)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        return json.loads(manifest_path.read_text(encoding="utf-8"))

manifests = {}
for relative, expected in checks:
    manifest = expand(relative)
    manifests[relative] = manifest
    runs = manifest["runs"]
    if len(runs) != expected:
        raise SystemExit(f"{relative}: expected {expected} runs, got {len(runs)}")
    if manifest.get("record_schema_version") != 3:
        raise SystemExit(f"{relative}: expected ExperimentRecord schema v3")
    if any(run["workers"] > 6 for run in runs):
        raise SystemExit(f"{relative}: exceeds six-worker ceiling")
    if any(run["parameters"].get("execution_backend") != "wasm" for run in runs):
        raise SystemExit(f"{relative}: Phase 4 must use real Wasm")
    if {int(run["parameters"]["sim.block_size"]) for run in runs} - {32, 128, 512}:
        raise SystemExit(f"{relative}: unexpected block size")

system = manifests[checks[0][0]]["runs"]
if {run["parameters"]["complexity"] for run in system} != {"light", "medium", "heavy"}:
    raise SystemExit("Phase 4 system matrix must retain light/medium/heavy real-Wasm complexity")
if {run["parameters"]["prediction_quality"] for run in system} != {"exact", "bucketed"}:
    raise SystemExit("Phase 4 system prediction axis must be exact + bucketed")
if {run["parameters"]["acg.serial_bypass_enabled"] for run in system} != {"false", "true"}:
    raise SystemExit("Phase 4 system must compare admission/bypass off vs on")
if {run["parameters"]["vm_instance_lifecycle"] for run in system} != {"reuse"}:
    raise SystemExit("Phase 4 production-system matrix must default to VM reuse")

vm = manifests[checks[1][0]]["runs"]
if {run["parameters"]["vm_instance_lifecycle"] for run in vm} != {"reuse", "recycle"}:
    raise SystemExit("Phase 4 VM matrix must compare reuse and recycle")
if {run["parameters"]["complexity"] for run in vm} != {"light", "medium", "heavy"}:
    raise SystemExit("Phase 4 VM matrix must retain all three complexity tiers")

mixed = manifests[checks[2][0]]["runs"]
if {run["parameters"]["complexity_mix"] for run in mixed} != {"80-15-5", "33-34-33", "10-30-60"}:
    raise SystemExit("Phase 4 mixed matrix must retain all three heterogeneous mixes")
if {run["parameters"]["acg.serial_bypass_enabled"] for run in mixed} != {"true"}:
    raise SystemExit("Phase 4 mixed matrix evaluates the production admission path")

exploration = manifests[checks[3][0]]["runs"]
settings = {
    (
        run["parameters"]["acg.exploration_rate"],
        run["parameters"]["acg.exploration_min_uncertainty"],
        run["parameters"]["acg.exploration_max_transactions_per_block"],
    )
    for run in exploration
}
if settings != {("0.00", "0.35", "0"), ("0.50", "0.50", "4"), ("0.50", "0.35", "8")}:
    raise SystemExit(f"unexpected Phase 4 targeted-exploration settings: {sorted(settings)}")

print("Phase 4 matrices PASS: 432 system + 36 VM + 216 mixed + 48 exploration = 732 runs")
PY

echo '=== Phase 4 Python tooling ==='
python3 -m py_compile \
  "$ROOT/scripts/summarize-conflictlab-phase4.py" \
  "$ROOT/scripts/validate-phase4-vm-equivalence.py" \
  "$ROOT/scripts/generate-manifest-matrix.py" \
  "$ROOT/scripts/aggregate-experiment.py"
"$ROOT/scripts/run-evaluation-tools-tests.sh"

echo 'PASS: Phase 4 control-plane diagnostics'
