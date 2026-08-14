#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}"

echo '=== formatting ==='
cargo fmt --manifest-path "$ROOT/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/runtime/Cargo.toml" --all -- --check
cargo fmt --manifest-path "$ROOT/benchmarks/Cargo.toml" --all -- --check

echo '=== feedback batching ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-feedback --test statistics \
  same_relationship_observations_are_mutated_once_but_raw_counts_and_weight_are_preserved -- --nocapture
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-feedback --test statistics \
  serialization_cost_batch_mutates_once_and_preserves_raw_count_and_mean -- --nocapture

echo '=== hard-DAG transitive reduction ==='
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-candidate-graph --lib \
  dense_hard_dag_is_transitively_reduced_without_changing_reachability -- --nocapture

echo '=== evaluation schema / acceptance ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-evaluation --all-targets -- --nocapture

echo '=== harness prediction-quality modes ==='
cargo test --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-benchmark-harness --test harness \
  conflictlab_prediction_quality_modes_expose_soft_edges_and_runtime_misses -- --nocapture

echo '=== ConflictLab contract ==='
cargo test --manifest-path "$ROOT/benchmarks/Cargo.toml" -p acg-benchmark-conflictlab -- --nocapture

echo '=== evaluation tooling ==='
"$ROOT/scripts/run-evaluation-tools-tests.sh"

echo '=== clippy root ==='
cargo clippy --manifest-path "$ROOT/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== clippy runtime ==='
cargo clippy --manifest-path "$ROOT/runtime/Cargo.toml" --workspace --all-targets -- -D warnings

echo '=== clippy benchmarks ==='
cargo clippy --manifest-path "$ROOT/benchmarks/Cargo.toml" --workspace --all-targets -- -D warnings

echo 'PASS: control-plane corrections diagnostics'
