#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

GO_TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
MOD_DIR="benchmarks/cosmos-wasmd-blockstm-s3"
LIB="runtime/target/release/libacg_wasmd_scheduler_ffi.a"
OUT="${VEGETA_S3_WASMD_SYMBGRAPH_BIN:-/tmp/vegeta-s3-wasmd-symbgraph-rust}"

command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 2; }
command -v go >/dev/null 2>&1 || { echo "go is required" >&2; exit 2; }

echo "[1/5] preserving authoritative crates/acg-* unit tests"
cargo test --workspace

echo "[2/5] testing the Rust/Go scheduler boundary crate"
cargo test --manifest-path runtime/Cargo.toml -p acg-wasmd-scheduler-ffi

echo "[3/5] building genuine Rust staticlib"
cargo build --manifest-path runtime/Cargo.toml -p acg-wasmd-scheduler-ffi --release
[[ -s "$LIB" ]] || { echo "missing or empty $LIB" >&2; exit 3; }
if command -v ar >/dev/null 2>&1; then
  members="$(ar t "$LIB" | wc -l)"
  (( members > 1 )) || { echo "$LIB does not look like a real Rust static archive" >&2; exit 3; }
fi

echo "[4/5] running Go integration tests against the Rust staticlib"
(cd "$MOD_DIR" && CGO_ENABLED=1 GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test -tags acg_rust ./...)

echo "[5/5] building Wasmd publication harness with Rust ACG enabled"
(cd "$MOD_DIR" && CGO_ENABLED=1 GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go build -tags acg_rust -o "$OUT" .)

echo "PASS: Rust ACG -> Go Wasmd scheduler bridge built at $OUT"
