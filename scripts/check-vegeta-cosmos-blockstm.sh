#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/benchmarks/cosmos-blockstm-s3"

TOOLCHAIN="${VEGETA_S3_COSMOS_GO_TOOLCHAIN:-auto}"
command -v go >/dev/null 2>&1 || { echo "Go is required" >&2; exit 2; }

echo "host go: $(go version)"
SELECTED="$(GOTOOLCHAIN="$TOOLCHAIN" go env GOVERSION)"
echo "selected Cosmos harness toolchain: $SELECTED (GOTOOLCHAIN=$TOOLCHAIN)"
echo "downloading Cosmos SDK v0.54.4 dependencies..."
GOTOOLCHAIN="$TOOLCHAIN" GOFLAGS=-mod=mod go mod download

echo "verifying public Cosmos Block-STM TxRunner package..."
GOTOOLCHAIN="$TOOLCHAIN" GOFLAGS=-mod=mod go list github.com/cosmos/cosmos-sdk/baseapp/txnrunner >/dev/null

echo "running Cosmos Block-STM access-replay unit tests..."
GOTOOLCHAIN="$TOOLCHAIN" GOFLAGS=-mod=mod go test ./...

echo "building Cosmos Block-STM access-replay harness..."
GOTOOLCHAIN="$TOOLCHAIN" GOFLAGS=-mod=mod go build ./...

echo "PASS: Cosmos SDK v0.54.4 baseapp/txnrunner Block-STM harness builds and tests with $SELECTED"
