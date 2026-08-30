#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
GO_TOOLCHAIN="${ACG_GO_TOOLCHAIN:-auto}"

run_workspace() {
  local manifest="$1" label="$2" format_mode="${3:-all}"
  if [[ "$format_mode" == "all" ]]; then
    echo "=== cargo fmt: $label (fix) ==="
    cargo fmt --manifest-path "$manifest" --all
    echo "=== cargo fmt: $label (verify) ==="
    cargo fmt --manifest-path "$manifest" --all -- --check
  elif [[ "$format_mode" == "non-frozen-benchmarks" ]]; then
    # The native-S3 contract sources are frozen symbolic-analysis evidence: their raw
    # SHA-256 and source-line windows are checked into benchmarks/symbolic/native-s3.
    # Formatting those crates would mutate the evidence corpus and invalidate provenance.
    local packages=(
      acg-benchmark-conflictlab
      acg-benchmark-miniwarehouse
      acg-benchmark-vegeta-trace
    )
    local package_args=()
    local package
    for package in "${packages[@]}"; do package_args+=(--package "$package"); done
    echo "=== cargo fmt: $label non-frozen crates (fix) ==="
    cargo fmt --manifest-path "$manifest" "${package_args[@]}"
    echo "=== cargo fmt: $label non-frozen crates (verify) ==="
    cargo fmt --manifest-path "$manifest" "${package_args[@]}" -- --check
  else
    echo "unknown cargo fmt mode: $format_mode" >&2
    exit 2
  fi
  echo "=== cargo test: $label (all targets) ==="
  cargo test --manifest-path "$manifest" --workspace --all-targets
  echo "=== cargo test: $label (doctests) ==="
  cargo test --manifest-path "$manifest" --workspace --doc
  if [[ "$format_mode" == "non-frozen-benchmarks" ]]; then
    echo "=== cargo clippy: $label non-frozen crates ==="
    cargo clippy --manifest-path "$manifest" "${package_args[@]}" --all-targets -- -D warnings
  else
    echo "=== cargo clippy: $label ==="
    cargo clippy --manifest-path "$manifest" --workspace --all-targets -- -D warnings
  fi
}

echo '=== repository syntax checks ==='
while IFS= read -r -d '' script; do bash -n "$script"; done < <(find . -type f -name '*.sh' -not -path './target/*' -not -path './*/target/*' -print0)
while IFS= read -r -d '' script; do python3 -m py_compile "$script"; done < <(find . -type f -name '*.py' -not -path './target/*' -not -path './*/target/*' -print0)

run_workspace "$ROOT/Cargo.toml" core
run_workspace "$ROOT/runtime/Cargo.toml" runtime
run_workspace "$ROOT/benchmarks/Cargo.toml" benchmark-contracts non-frozen-benchmarks

echo '=== repository whitespace check after cargo fmt ==='
git diff --check

echo '=== Python evaluation/tooling unit tests ==='
python3 -m unittest discover -s "$ROOT/tools/tests" -p 'test_*.py' -v

echo '=== Cosmos SDK access-replay Go unit tests ==='
(
  cd "$ROOT/benchmarks/cosmos-blockstm-s3"
  GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test ./...
)

echo '=== Rust ACG staticlib for Wasmd integration tests ==='
cargo build --manifest-path "$ROOT/runtime/Cargo.toml" -p acg-wasmd-scheduler-ffi --release
[[ -s "$ROOT/runtime/target/release/libacg_wasmd_scheduler_ffi.a" ]] || { echo 'missing Rust ACG staticlib' >&2; exit 3; }

echo '=== Wasmd/WasmVM Go unit tests (stub and Rust-ACG bridge) ==='
(
  cd "$ROOT/benchmarks/cosmos-wasmd-blockstm-s3"
  GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test ./...
  CGO_ENABLED=1 GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test -tags acg_rust ./...
)

echo '=== ALL SYSTEM TESTS PASSED ==='
