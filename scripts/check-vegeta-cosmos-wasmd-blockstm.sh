#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
MOD_DIR="benchmarks/cosmos-wasmd-blockstm-s3"
GO_TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
BIN="${VEGETA_S3_WASMD_BLOCKSTM_CHECK_BIN:-/tmp/vegeta-s3-cosmos-wasmd-blockstm-check}"

for p in "$EXEC_DIR/execution-manifest.json" "$EXEC_DIR/execution-plan.jsonl"; do
  [[ -s "$p" ]] || { echo "missing required generated native S3 input: $p" >&2; exit 2; }
done
command -v go >/dev/null 2>&1 || { echo "Go is required" >&2; exit 2; }
command -v cargo >/dev/null 2>&1 || { echo "cargo is required to build the native S3 Wasm artifacts" >&2; exit 2; }

echo "host go: $(go version)"
echo "selected toolchain: $(cd "$MOD_DIR" && GOTOOLCHAIN="$GO_TOOLCHAIN" go env GOVERSION) (GOTOOLCHAIN=$GO_TOOLCHAIN)"
echo "building native S3 Wasm artifacts..."
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown

echo "downloading Wasmd/Cosmos SDK dependencies..."
(cd "$MOD_DIR" && GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go mod download)
echo "running Wasmd Block-STM harness unit tests..."
(cd "$MOD_DIR" && GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go test ./...)
echo "building Wasmd Block-STM harness..."
(cd "$MOD_DIR" && GOTOOLCHAIN="$GO_TOOLCHAIN" GOFLAGS=-mod=mod go build -o "$BIN" .)

echo "running real Wasmd/WasmVM setup smoke (upload + instantiate + priming)..."
"$BIN" \
  --repo-root "$ROOT" \
  --manifest "$EXEC_DIR/execution-manifest.json" \
  --plan "$EXEC_DIR/execution-plan.jsonl" \
  --setup-only

echo "PASS: Wasmd v0.70.3 + Cosmos SDK v0.54.4 TxRunner Block-STM harness builds, tests, and initializes the native S3 Wasm workload"
