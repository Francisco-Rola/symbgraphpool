#!/usr/bin/env bash
set -euo pipefail

DIR="${1:-benchmarks/corpora/vegeta-ethereum/s3/native-execution/publication-matrix-wasmd-prepared-w2/compare-pprof}"

for p in \
  "$DIR/cosmos-wasmd-direct-serial-w2.cpu.pprof" \
  "$DIR/cosmos-wasmd-outer-cache-serial-w2.cpu.pprof" \
  "$DIR/cosmos-wasmd-symbgraph-static-w2.cpu.pprof"; do
  [[ -s "$p" ]] || { echo "missing profile: $p" >&2; exit 2; }
done

echo '=== CPU: direct serial ==='
go tool pprof -top "$DIR/cosmos-wasmd-direct-serial-w2.cpu.pprof"
echo '=== CPU: outer-cache serial ==='
go tool pprof -top "$DIR/cosmos-wasmd-outer-cache-serial-w2.cpu.pprof"
echo '=== CPU differential: outer-cache minus direct ==='
go tool pprof -top \
  -diff_base="$DIR/cosmos-wasmd-direct-serial-w2.cpu.pprof" \
  "$DIR/cosmos-wasmd-outer-cache-serial-w2.cpu.pprof"
echo '=== CPU: SymbGraph ==='
go tool pprof -top "$DIR/cosmos-wasmd-symbgraph-static-w2.cpu.pprof"

echo '=== execution allocation profile: direct serial ==='
go tool pprof -top \
  -diff_base="$DIR/cosmos-wasmd-direct-serial-w2.allocs-before.pprof" \
  "$DIR/cosmos-wasmd-direct-serial-w2.allocs-after.pprof"
echo '=== execution allocation profile: outer-cache serial ==='
go tool pprof -top \
  -diff_base="$DIR/cosmos-wasmd-outer-cache-serial-w2.allocs-before.pprof" \
  "$DIR/cosmos-wasmd-outer-cache-serial-w2.allocs-after.pprof"
echo '=== execution allocation profile: SymbGraph ==='
go tool pprof -top \
  -diff_base="$DIR/cosmos-wasmd-symbgraph-static-w2.allocs-before.pprof" \
  "$DIR/cosmos-wasmd-symbgraph-static-w2.allocs-after.pprof"

for runner in direct-serial outer-cache-serial symbgraph-static; do
  echo "=== mutex: $runner ==="
  go tool pprof -top "$DIR/cosmos-wasmd-${runner}-w2.mutex.pprof"
done

echo '=== profile metadata ==='
for p in "$DIR"/*.profile.json; do
  echo "--- $p"
  cat "$p"
done
