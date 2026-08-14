#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

echo '=== formatting ==='
cargo fmt --manifest-path "$ROOT/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/runtime/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/benchmarks/Cargo.toml" --all -- --check

echo '=== upstream feedback aggregation ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-feedback --test statistics \
  same_relationship_observations_are_mutated_once_but_raw_counts_and_weight_are_preserved -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-feedback --test statistics \
  upstream_aggregate_matches_pairwise_observation_application -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-feedback --test statistics \
  serialization_cost_batch_mutates_once_and_preserves_raw_count_and_mean -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --test runtime_feedback \
  aggregated_collector_matches_pairwise_candidate_feedback -- --nocapture
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-runtime-feedback --test runtime_feedback \
  aggregated_collector_matches_pairwise_fallback_negative_cardinality -- --nocapture

echo '=== candidate adjacency + hard-DAG transitive reduction ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --test candidate_graph \
  exact_input_keys_prune_same_profile_cartesian_pairs -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  dense_hard_dag_is_transitively_reduced_without_changing_reachability -- --nocapture

echo '=== evaluation schema / pipeline acceptance ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-evaluation --all-targets -- --nocapture

echo '=== harness prediction-quality modes ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  conflictlab_prediction_quality_modes_expose_soft_edges_and_runtime_misses -- --nocapture

echo '=== ConflictLab contract ==='
cargo test --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab -- --nocapture

echo '=== production matrix definitions ==='
python3 - "$ROOT" <<'PY'
import json
import subprocess
import sys
import tempfile
from pathlib import Path

root = Path(sys.argv[1])
expected_sizes = {16, 32, 64, 128, 256, 512}
checks = [
    ("evaluation/conflictlab/control-plane-regression.grid.json", 108),
    ("evaluation/conflictlab/forced-speculation.grid.json", 432),
]

for relative, expected_runs in checks:
    grid = root / relative
    with tempfile.TemporaryDirectory() as temp:
        manifest_path = Path(temp) / "manifest.json"
        subprocess.run(
            [sys.executable, str(root / "scripts/generate-manifest-matrix.py"), str(grid), str(manifest_path)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    runs = manifest["runs"]
    if len(runs) != expected_runs:
        raise SystemExit(f"{relative}: expected {expected_runs} runs, got {len(runs)}")
    sizes = {int(run["parameters"]["sim.block_size"]) for run in runs}
    if sizes != expected_sizes:
        raise SystemExit(f"{relative}: block sizes {sorted(sizes)} != {sorted(expected_sizes)}")
    if any(int(run["parameters"]["transactions"]) != int(run["parameters"]["sim.block_size"]) for run in runs):
        raise SystemExit(f"{relative}: measured transactions must equal block size")
    if any(run["workers"] > 6 for run in runs):
        raise SystemExit(f"{relative}: worker count exceeds six-core evaluation ceiling")
    if manifest["record_schema_version"] != 3:
        raise SystemExit(f"{relative}: expected ExperimentRecord schema 3")

risk_grid = json.loads((root / "evaluation/conflictlab/forced-speculation.grid.json").read_text(encoding="utf-8"))
if risk_grid["base_run"]["parameters"].get("prediction_quality") != "coarse":
    raise SystemExit("coarse policy matrix must use prediction_quality=coarse")
if set(risk_grid["matrix"]["parameters"].get("acg.risk_budget", [])) != {"0.50", "0.75", "0.90"}:
    raise SystemExit("coarse policy matrix risk-budget sweep changed unexpectedly")

# No ConflictLab block-size matrix should ask the execution harness for a block larger than 512.
for grid in sorted((root / "evaluation/conflictlab").glob("*.grid.json")):
    data = json.loads(grid.read_text(encoding="utf-8"))
    values = []
    base = data.get("base_run", {}).get("parameters", {})
    if "sim.block_size" in base:
        values.append(int(base["sim.block_size"]))
    matrix = data.get("matrix", {})
    values.extend(int(value) for value in matrix.get("parameters", {}).get("sim.block_size", []))
    for case in matrix.get("cases", []):
        value = case.get("parameters", {}).get("sim.block_size")
        if value is not None:
            values.append(int(value))
    if values and max(values) > 512:
        raise SystemExit(f"{grid.relative_to(root)} requests block size {max(values)} > 512")

print("matrix checks PASS: 108 production-scaling runs + 432 coarse-policy runs; block sizes 16..512")
PY

echo '=== evaluation tooling ==='
"$ROOT/scripts/run-evaluation-tools-tests.sh"

echo '=== clippy root ==='
cargo clippy --manifest-path "$ROOT/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== clippy runtime ==='
cargo clippy --manifest-path "$ROOT/runtime/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== clippy benchmarks ==='
cargo clippy --manifest-path "$ROOT/benchmarks/Cargo.toml" --workspace --all-targets -- -D warnings

echo 'PASS: control-plane v3 diagnostics'
