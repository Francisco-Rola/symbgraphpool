#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-$ROOT/benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
mkdir -p "$OUT"
MANIFEST="evaluation/vegeta/s1-native-implementation-manifest.v1.json"
python3 "$ROOT/tools/vegeta/validate-native-s3-implementation.py" \
  --repo-root "$ROOT" --manifest "$MANIFEST" \
  --json-output "${OUT#$ROOT/}/native-implementation-validation.json" \
  --text-output "${OUT#$ROOT/}/native-implementation-validation.txt"

packages=(
  acg-benchmark-native-s3-cw20-base acg-benchmark-native-s3-controlled-cw20 acg-benchmark-native-s3-fiat-token-cw20
  acg-benchmark-native-s3-fee-token-cw20 acg-benchmark-native-s3-wrapped-native-token
  acg-benchmark-native-s3-cw721-mintable acg-benchmark-native-s3-astroport-pair
  acg-benchmark-native-s3-xen-like acg-benchmark-native-s3-cw1155-like
  acg-benchmark-native-s3-marketplace-router acg-benchmark-native-s3-operator-filter-helper
  acg-benchmark-native-s3-cw721-drop acg-benchmark-native-s3-stargate-cw20
)
for package in "${packages[@]}"; do
  cargo test --manifest-path "$ROOT/benchmarks/Cargo.toml" -p "$package"
  cargo build --manifest-path "$ROOT/benchmarks/Cargo.toml" -p "$package" --release --target wasm32-unknown-unknown
done
cargo test --manifest-path "$ROOT/Cargo.toml" -p acg-symbolic-json native_s3_artifacts -- --nocapture
python3 "$ROOT/tools/vegeta/validate-native-s3-implementation.py" \
  --repo-root "$ROOT" --manifest "$MANIFEST" --require-wasm-artifacts \
  --json-output "${OUT#$ROOT/}/native-implementation-validation.json" \
  --text-output "${OUT#$ROOT/}/native-implementation-validation.txt"
echo "PASS: Vegeta S1 reviewed native contracts and source-derived symbolic analyses are implementation-ready"
