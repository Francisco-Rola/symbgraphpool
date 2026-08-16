#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

run_workspace() {
  local manifest="$1"
  local label="$2"
  echo "=== cargo test: ${label} (all targets) ==="
  cargo test --manifest-path "$manifest" --workspace --all-targets
  echo "=== cargo test: ${label} (doctests) ==="
  cargo test --manifest-path "$manifest" --workspace --doc
  echo "=== cargo clippy: ${label} ==="
  cargo clippy --manifest-path "$manifest" --workspace --all-targets -- -D warnings
}

echo '=== cargo fmt: root workspace ==='
cargo fmt --manifest-path "$ROOT/Cargo.toml" --all

echo '=== cargo fmt: runtime workspace ==='
cargo fmt --manifest-path "$ROOT/runtime/Cargo.toml" --all

echo '=== cargo fmt: benchmark/contracts workspace ==='
cargo fmt --manifest-path "$ROOT/benchmarks/Cargo.toml" --all

echo '=== git diff --check ==='
git -C "$ROOT" diff --check

echo '=== shell syntax: every repository shell script ==='
while IFS= read -r -d '' script; do
  bash -n "$script"
done < <(find "$ROOT" -type f -name '*.sh' -not -path '*/target/*' -print0)

echo '=== Python syntax: every repository Python script ==='
while IFS= read -r -d '' script; do
  python3 -m py_compile "$script"
done < <(find "$ROOT" -type f -name '*.py' -not -path '*/target/*' -print0)

run_workspace "$ROOT/Cargo.toml" 'core'
run_workspace "$ROOT/runtime/Cargo.toml" 'runtime'
run_workspace "$ROOT/benchmarks/Cargo.toml" 'benchmark contracts'

echo '=== evaluation-tool regression suite ==='
python3 "$ROOT/scripts/tests/test_evaluation_tools.py"

echo '=== ALL REPOSITORY TESTS PASSED ==='
