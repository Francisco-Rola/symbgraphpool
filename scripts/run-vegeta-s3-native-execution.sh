#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PLAN_DIR="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}"
EXEC_DIR="${VEGETA_S3_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-execution}"
CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
CHAR_DIR="${VEGETA_S3_CHARACTERIZATION:-benchmarks/corpora/vegeta-ethereum/s3/characterization}"

FAMILY_MAP="${VEGETA_S3_NATIVE_FAMILY_MAP:-evaluation/vegeta/s3-native-family-map.v2.json}"
for path in "$PLAN_DIR/selector-semantic-map.json" "$CHAR_DIR/code-cache.json" "$CHAR_DIR/native-family-mapping-candidates.json" "$CORPUS" "$FAMILY_MAP"; do
  if [[ ! -s "$path" ]]; then echo "missing required Vegeta S3 input: $path" >&2; exit 2; fi
done
if [[ ! -d "$CHAR_DIR/call-cache" ]]; then
  echo "missing required Vegeta S3 callTracer cache: $CHAR_DIR/call-cache" >&2
  exit 2
fi

# native-plan.jsonl is derived data. Refresh it from the authoritative callTracer cache so fields
# added by newer planner versions (notably ethereum_caller=geth callTracer `from`) cannot remain
# stale across executor iterations.
if [[ "${VEGETA_S3_REFRESH_NATIVE_PLAN:-1}" == "1" ]]; then
  echo "refreshing Vegeta S3 native plan from callTracer cache"
  python3 scripts/vegeta/build-native-s3-plan.py \
    --corpus "$CORPUS" \
    --characterization-dir "$CHAR_DIR" \
    --family-map "$FAMILY_MAP" \
    --output-dir "$PLAN_DIR"
fi
if [[ ! -s "$PLAN_DIR/native-plan.jsonl" ]]; then
  echo "missing required Vegeta S3 input: $PLAN_DIR/native-plan.jsonl" >&2
  exit 2
fi

bash scripts/run-vegeta-s3-native-implementation-validation.sh "$PLAN_DIR"

INITIAL_STATE_MODE="${VEGETA_S3_NATIVE_INITIAL_STATE_MODE:-rpc}"
if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "native S3 publication replay requires predecessor-block logical state; set ETH_RPC_URL to an archive-capable Ethereum RPC (or explicitly set VEGETA_S3_NATIVE_INITIAL_STATE_MODE=heuristic for diagnostics only)" >&2
  exit 2
fi

PREP_ARGS=(
  --plan "$PLAN_DIR/native-plan.jsonl"
  --selector-map "$PLAN_DIR/selector-semantic-map.json"
  --code-cache "$CHAR_DIR/code-cache.json"
  --output-dir "$EXEC_DIR"
  --initial-state-mode "$INITIAL_STATE_MODE"
  --caller-mode exact
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP_ARGS+=(--rpc-url "$ETH_RPC_URL"); fi
python3 scripts/vegeta/prepare-native-s3-execution.py "${PREP_ARGS[@]}"

# Build all native workload contracts as real Wasm and the host-side atomic bundle executor.
cargo build --manifest-path benchmarks/Cargo.toml --workspace --release --target wasm32-unknown-unknown
cargo build --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --release

runtime/target/release/acg-vegeta-native-s3-executor \
  --repo-root "$ROOT" \
  --manifest "$EXEC_DIR/execution-manifest.json" \
  --plan "$EXEC_DIR/execution-plan.jsonl" \
  --output "$EXEC_DIR/native-accesses.jsonl"

python3 scripts/vegeta/measure-native-s3-fidelity.py \
  --corpus "$CORPUS" \
  --native-accesses "$EXEC_DIR/native-accesses.jsonl" \
  --output-dir "$EXEC_DIR"
python3 scripts/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR"

echo "PASS: Vegeta S3 native bundle execution and topology fidelity measurement completed"
