#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

echo '=== Phase 5 formatting ==='
cargo fmt --manifest-path "$ROOT/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/runtime/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/benchmarks/Cargo.toml" --all -- --check

echo '=== Phase 5 hard + mature-soft compact planning ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --test weighted_candidate_graph \
  immature_equivalence_clique_is_materialized_as_a_chain_with_logical_coverage -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --test weighted_candidate_graph \
  mature_soft_equivalence_clique_stays_compact_with_pairwise_schedule_semantics -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  mature_bucketed_soft_relationships_remain_compact_after_feedback -- --nocapture

echo '=== Phase 5 group-level runtime feedback equivalence ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --test runtime_feedback \
  aggregated_collector_matches_pairwise_candidate_feedback -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --test runtime_feedback \
  aggregated_collector_matches_pairwise_fallback_negative_cardinality -- --nocapture

echo '=== Phase 5 serial admission / true direct bypass ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --lib \
  serial_bypass_config_validates_ema_and_hysteresis -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --lib \
  serial_bypass_hysteresis_requires_a_stronger_signal_to_exit -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  serial_bypass_skips_candidate_graph_after_losing_warmup_economics -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-evaluation --test acceptance \
  schema_v3_acceptance_understands_true_serial_bypass_execution -- --nocapture

echo '=== Phase 5 reusable Wasm regression ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-cosmwasm-engine --test wasm_smoke -- --nocapture

echo '=== Phase 5 matrix contract (no exploration study) ==='
python3 - "$ROOT" <<'PY'
import json
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
checks = [
    ("evaluation/conflictlab/phase5-control-plane.grid.json", 432),
    ("evaluation/conflictlab/phase5-mixed-admission.grid.json", 48),
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
        raise SystemExit(f"{relative}: Phase 5 must use real Wasm")
    if any(key.startswith("acg.exploration_") for run in runs for key in run["parameters"]):
        raise SystemExit(f"{relative}: Phase 5 must not carry an exploration study")
    for run in runs:
        p = run["parameters"]
        if p.get("vm_instance_lifecycle") != "reuse":
            raise SystemExit(f"{relative}: reusable VM lifecycle is the Phase 5 default")
        if p.get("acg.serial_bypass_economics_ema_alpha") != "0.35":
            raise SystemExit(f"{relative}: EMA admission alpha changed")
        if p.get("acg.serial_bypass_min_economics_observations") != "4":
            raise SystemExit(f"{relative}: admission observation floor changed")
        if p.get("acg.serial_bypass_projected_speedup_hysteresis") != "0.10":
            raise SystemExit(f"{relative}: admission hysteresis changed")

control = manifests[checks[0][0]]["runs"]
if {int(run["parameters"]["sim.block_size"]) for run in control} != {32, 128, 512}:
    raise SystemExit("Phase 5 control matrix must retain B32/B128/B512")
if {run["parameters"]["complexity"] for run in control} != {"light", "medium", "heavy"}:
    raise SystemExit("Phase 5 control matrix must retain light/medium/heavy")
if {run["parameters"]["prediction_quality"] for run in control} != {"exact", "bucketed"}:
    raise SystemExit("Phase 5 control matrix must retain exact + bucketed prediction")
if {run["parameters"]["acg.serial_bypass_enabled"] for run in control} != {"false", "true"}:
    raise SystemExit("Phase 5 control matrix must compare admission off/on")
if {run["mode"] for run in control} != {"static", "probability-only", "cost-aware"}:
    raise SystemExit("Phase 5 control matrix must retain the three scheduling modes")

mixed = manifests[checks[1][0]]["runs"]
if {int(run["parameters"]["sim.block_size"]) for run in mixed} != {128, 512}:
    raise SystemExit("Phase 5 mixed admission matrix must use B128/B512")
if {run["parameters"]["complexity_mix"] for run in mixed} != {"80-15-5", "33-34-33", "10-30-60"}:
    raise SystemExit("Phase 5 mixed matrix must retain all three heterogeneous mixes")
if {run["mode"] for run in mixed} != {"cost-aware"}:
    raise SystemExit("Phase 5 mixed matrix is a focused production-admission validation")

print("Phase 5 matrices PASS: 432 control-plane + 48 mixed-admission = 480 runs; no exploration study")
PY

echo '=== Phase 5 Python tooling ==='
python3 -m py_compile \
  "$ROOT/scripts/validate-conflictlab-phase5.py" \
  "$ROOT/scripts/summarize-conflictlab-phase5.py" \
  "$ROOT/scripts/generate-manifest-matrix.py" \
  "$ROOT/scripts/aggregate-experiment.py"
"$ROOT/scripts/run-evaluation-tools-tests.sh"

echo '=== Phase 5 clippy root ==='
cargo clippy --manifest-path "$ROOT/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== Phase 5 clippy runtime ==='
cargo clippy --manifest-path "$ROOT/runtime/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== Phase 5 clippy benchmarks ==='
cargo clippy --manifest-path "$ROOT/benchmarks/Cargo.toml" --workspace --all-targets -- -D warnings

echo 'PASS: Phase 5 control-plane diagnostics'
