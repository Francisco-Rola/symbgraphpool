#!/usr/bin/env bash

set -euo pipefail

export RUST_BACKTRACE=1

echo
echo "============================================================"
echo " PRE-BRICK-4 ACCEPTANCE"
echo "============================================================"

echo
echo "== Toolchain =="
rustc --version
cargo --version
rustup show active-toolchain

rustc --version | grep -q '^rustc 1\.75\.0 ' || {
    echo "ERROR: expected Rust 1.75.0"
    exit 1
}

echo
echo "== Repository whitespace sanity =="
git diff --check

echo
echo "== Cargo metadata =="
cargo metadata --no-deps --format-version 1 >/dev/null
cargo metadata \
  --manifest-path runtime/Cargo.toml \
  --no-deps \
  --format-version 1 >/dev/null
cargo metadata \
  --manifest-path benchmarks/Cargo.toml \
  --no-deps \
  --format-version 1 >/dev/null

echo
echo "============================================================"
echo " BRICK 3 FOCUSED TESTS"
echo "============================================================"

cargo test \
  -p acg-feedback \
  --test statistics \
  -- \
  --nocapture

cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-runtime-feedback \
  --test runtime_feedback \
  -- \
  --nocapture

cargo test \
  --manifest-path runtime/Cargo.toml \
  -p acg-runtime-feedback \
  --test miniwarehouse_feedback \
  -- \
  --nocapture

cargo test \
  -p acg-profile-graph \
  --test graph \
  finds_profile_edge_by_dense_endpoint_pair_in_either_order \
  -- \
  --exact \
  --nocapture

echo
echo "============================================================"
echo " ROOT WORKSPACE"
echo "============================================================"

cargo fmt --all -- --check

cargo clippy \
  --workspace \
  --all-targets \
  -- \
  -D warnings

cargo test \
  --workspace \
  --all-targets

echo
echo "============================================================"
echo " RUNTIME WORKSPACE"
echo "============================================================"

cargo fmt \
  --manifest-path runtime/Cargo.toml \
  --all \
  -- \
  --check

cargo clippy \
  --manifest-path runtime/Cargo.toml \
  --workspace \
  --all-targets \
  -- \
  -D warnings

cargo test \
  --manifest-path runtime/Cargo.toml \
  --workspace \
  --all-targets

echo
echo "============================================================"
echo " BENCHMARK WORKSPACE"
echo "============================================================"

cargo fmt \
  --manifest-path benchmarks/Cargo.toml \
  --all \
  -- \
  --check

cargo clippy \
  --manifest-path benchmarks/Cargo.toml \
  --workspace \
  --all-targets \
  -- \
  -D warnings

cargo test \
  --manifest-path benchmarks/Cargo.toml \
  --workspace

echo
echo "============================================================"
echo " MSRV DEPENDENCY PINS"
echo "============================================================"

cargo tree \
  --manifest-path benchmarks/Cargo.toml \
  -i 'base64ct@1.6.0' | grep 'base64ct v1.6.0'

cargo tree \
  --manifest-path benchmarks/Cargo.toml \
  -i 'zeroize@1.8.2' | grep 'zeroize v1.8.2'

cargo tree \
  --manifest-path runtime/Cargo.toml \
  -i 'base64ct@1.6.0' | grep 'base64ct v1.6.0'

cargo tree \
  --manifest-path runtime/Cargo.toml \
  -i 'zeroize@1.8.2' | grep 'zeroize v1.8.2'

cargo tree \
  --manifest-path runtime/Cargo.toml \
  -i 'indexmap@2.11.4' | grep 'indexmap v2.11.4'

cargo tree \
  --manifest-path runtime/Cargo.toml \
  -i 'clru@0.6.2' | grep 'clru v0.6.2'

echo
echo "============================================================"
echo " BENCHMARK WASM BUILD"
echo "============================================================"

if ! rustup target list --installed | grep -qx 'wasm32-unknown-unknown'; then
    echo "ERROR: wasm32-unknown-unknown is not installed."
    echo "Run:"
    echo "  rustup target add wasm32-unknown-unknown"
    exit 1
fi

cargo build \
  --manifest-path benchmarks/Cargo.toml \
  --workspace \
  --release \
  --target wasm32-unknown-unknown

echo
echo "============================================================"
echo " SYMBOLIC GRAPH REGRESSION"
echo "============================================================"

cargo build -p acg-cli
ACG=target/debug/acg-profilec

"$ACG" compile \
  --input benchmarks/symbolic/conflictlab.symbolic.json \
  --output /tmp/conflictlab-a.json \
  --runtime cosmwasm \
  --code-hash 1111111111111111111111111111111111111111111111111111111111111111

"$ACG" compile \
  --input benchmarks/symbolic/conflictlab.symbolic.json \
  --output /tmp/conflictlab-b.json \
  --runtime cosmwasm \
  --code-hash 1111111111111111111111111111111111111111111111111111111111111111

CONFLICTLAB_INSPECT="$("$ACG" inspect --input /tmp/conflictlab-a.json)"
printf '%s\n' "$CONFLICTLAB_INSPECT"

grep -Fxq 'profiles: 18' <<<"$CONFLICTLAB_INSPECT"
grep -Fxq 'edges: 63' <<<"$CONFLICTLAB_INSPECT"
grep -Fxq 'conditional_edges: 46' <<<"$CONFLICTLAB_INSPECT"
grep -Fxq 'unconditional_edges: 0' <<<"$CONFLICTLAB_INSPECT"
grep -Fxq 'unknown_edges: 17' <<<"$CONFLICTLAB_INSPECT"

cmp /tmp/conflictlab-a.json /tmp/conflictlab-b.json

"$ACG" compile \
  --input benchmarks/symbolic/miniwarehouse.symbolic.json \
  --output /tmp/miniwarehouse-a.json \
  --runtime cosmwasm \
  --code-hash 2222222222222222222222222222222222222222222222222222222222222222

"$ACG" compile \
  --input benchmarks/symbolic/miniwarehouse.symbolic.json \
  --output /tmp/miniwarehouse-b.json \
  --runtime cosmwasm \
  --code-hash 2222222222222222222222222222222222222222222222222222222222222222

MINIWAREHOUSE_INSPECT="$("$ACG" inspect --input /tmp/miniwarehouse-a.json)"
printf '%s\n' "$MINIWAREHOUSE_INSPECT"

grep -Fxq 'profiles: 14' <<<"$MINIWAREHOUSE_INSPECT"
grep -Fxq 'edges: 44' <<<"$MINIWAREHOUSE_INSPECT"
grep -Fxq 'conditional_edges: 38' <<<"$MINIWAREHOUSE_INSPECT"
grep -Fxq 'unconditional_edges: 0' <<<"$MINIWAREHOUSE_INSPECT"
grep -Fxq 'unknown_edges: 6' <<<"$MINIWAREHOUSE_INSPECT"

cmp /tmp/miniwarehouse-a.json /tmp/miniwarehouse-b.json

echo
echo "Deterministic hashes:"
sha256sum \
  /tmp/conflictlab-a.json \
  /tmp/conflictlab-b.json \
  /tmp/miniwarehouse-a.json \
  /tmp/miniwarehouse-b.json

echo
echo "============================================================"
echo " ALL PRE-BRICK-4 ACCEPTANCE CHECKS PASSED"
echo "============================================================"