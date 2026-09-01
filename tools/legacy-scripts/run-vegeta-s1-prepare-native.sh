#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

CORPUS="${VEGETA_S1_CORPUS:-benchmarks/corpora/vegeta-ethereum/s1/corpus.jsonl}"
WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
EXEC_DIR="${VEGETA_S1_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
FAMILY_MAP="${VEGETA_S1_NATIVE_FAMILY_MAP:-evaluation/vegeta/s1-native-family-map.v2.json}"
MINT_SEQUENCE="${VEGETA_S1_CW721_MINT_SEQUENCE:-$WORK_DIR/cw721-drop-mint-sequence.json}"
MIA_MINT_AUDIT="${VEGETA_S1_MIA_MINT_AUDIT:-$WORK_DIR/mia-fd883998-mint-audit.json}"
S3_CORPUS="${VEGETA_S3_CORPUS:-benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl}"
S3_PLAN="${VEGETA_S3_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s3/native-plan}/native-plan.jsonl"
MIN_CONFLICT="${VEGETA_S1_MIN_CONFLICT_COVERAGE:-0.95}"
MIN_MEDIAN_BLOCK="${VEGETA_S1_MIN_MEDIAN_BLOCK_COVERAGE:-0.80}"
MIN_SEM_TX="${VEGETA_S1_MIN_SEMANTIC_TX_COVERAGE:-0.80}"
MIN_SEM_FRAME="${VEGETA_S1_MIN_SEMANTIC_FRAME_COVERAGE:-0.00}"
MIN_CONTENTION_TX="${VEGETA_S1_MIN_CONTENTION_TX_COVERAGE:-0.80}"
READINESS_PROFILE="${VEGETA_S1_READINESS_PROFILE:-semantic-replay}"
ALLOW_LOW="${VEGETA_S1_ALLOW_LOW_COVERAGE:-0}"

# First refresh the reviewed owner map, build the native plan, and enforce the stronger selector-aware
# conflict gate. This stage is local-only when the expensive S1 code/callTracer caches already exist.
VEGETA_S1_MIN_CONFLICT_COVERAGE="$MIN_CONFLICT" \
VEGETA_S1_MIN_MEDIAN_BLOCK_COVERAGE="$MIN_MEDIAN_BLOCK" \
VEGETA_S1_ALLOW_LOW_COVERAGE="$ALLOW_LOW" \
bash tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh

READINESS_ARGS=(
  --translation-coverage "$PLAN_DIR/translation-coverage.json"
  --semantic-conflict-coverage "$PLAN_DIR/semantic-conflict-coverage.json"
  --transaction-deficit "$PLAN_DIR/transaction-deficit.json"
  --profile "$READINESS_PROFILE"
  --min-conflict "$MIN_CONFLICT"
  --min-median-block "$MIN_MEDIAN_BLOCK"
  --min-semantic-tx "$MIN_SEM_TX"
  --min-contention-tx "$MIN_CONTENTION_TX"
  --min-reviewed-state-frame "$MIN_SEM_FRAME"
  --output "$PLAN_DIR/readiness.json"
  --text-output "$PLAN_DIR/readiness.txt"
)
if [[ "$ALLOW_LOW" == "1" ]]; then READINESS_ARGS+=(--allow-low); fi
python3 tools/vegeta/evaluate-vegeta-s1-readiness.py "${READINESS_ARGS[@]}"

if [[ -s "$S3_CORPUS" && -s "$S3_PLAN" ]]; then
  PREFIX_ARGS=(--s1-corpus "$CORPUS" --s3-corpus "$S3_CORPUS" --s1-native-plan "$PLAN_DIR/native-plan.jsonl" --s3-native-plan "$S3_PLAN" --prefix-blocks 101)
  if [[ "${VEGETA_S1_STRICT_NATIVE_PREFIX:-0}" == "1" ]]; then PREFIX_ARGS+=(--strict-native-plan); fi
  python3 tools/vegeta/validate-vegeta-s1-prefix.py "${PREFIX_ARGS[@]}"
fi

# Before any Wasm build or historical state reconstruction, freeze the narrow public ERC721 mint
# event audit used to align state-derived sequential token IDs. Schema v2 also stores the indexed
# mint recipient, which is required for the handful of owner-scoped event-backed S1 mint effects.
# This is ~20 chunked eth_getLogs calls at the default 250-block chunk size and is reusable.
MINT_SEQUENCE_CURRENT=0
if [[ -s "$MINT_SEQUENCE" ]]; then
  if python3 - "$MINT_SEQUENCE" <<'PY_MINT_SCHEMA'
import json, sys
try:
    doc=json.load(open(sys.argv[1]))
    ok=int(doc.get("schema_version",0)) >= 2
    if ok:
        for owner in (doc.get("owners") or {}).values():
            for row in (owner.get("transactions") or {}).values():
                if len(row.get("recipients") or []) != int(row.get("mint_count",0)):
                    ok=False; break
            if not ok: break
except Exception:
    ok=False
raise SystemExit(0 if ok else 1)
PY_MINT_SCHEMA
  then MINT_SEQUENCE_CURRENT=1; fi
fi
if [[ "$MINT_SEQUENCE_CURRENT" != "1" || "${VEGETA_S1_REFRESH_CW721_MINT_SEQUENCE:-0}" == "1" ]]; then
  [[ -n "${ETH_RPC_URL:-}" ]] || { echo "S1 cw721-drop mint-sequence schema v2 is missing/stale. Set ETH_RPC_URL once to refresh public ERC721 mint logs: $MINT_SEQUENCE" >&2; exit 2; }
  VEGETA_S1_CW721_MINT_SEQUENCE="$MINT_SEQUENCE" \
    bash tools/legacy-scripts/run-vegeta-s1-cw721-mint-audit.sh
else
  echo "reusing S1 cw721-drop mint sequence schema v2: $MINT_SEQUENCE"
fi

# Reconcile the current reviewed mint adapters against the frozen public event sequence before any
# Wasm build or 739k-transaction execution-plan construction. This catches selector/cardinality
# mistakes cheaply and uses the same adapter implementation as full preparation.
bash tools/legacy-scripts/run-vegeta-s1-cw721-drop-translation-audit.sh
python3 - "$PLAN_DIR/cw721-drop-translation-audit.json" <<'PY_MINT_RECONCILE'
import json, sys
doc=json.load(open(sys.argv[1]))
summary=doc.get("summary") or {}
issues=int(summary.get("issue_transactions", -1))
if issues != 0:
    raise SystemExit(
        f"S1 cw721-drop translation reconciliation still has {issues} issue transactions; "
        f"inspect {sys.argv[1]} before full native preparation"
    )
print("PASS: S1 cw721-drop committed mint translation reconciles exactly with public Transfer events")
PY_MINT_RECONCILE

# MIA's reviewed 0xfd883998 mint effect is non-sequential, so native execution consumes the
# owner-scoped public Transfer-log audit rather than guessing a calldata token-ID layout.
if [[ ! -s "$MIA_MINT_AUDIT" || "${VEGETA_S1_REFRESH_MIA_MINT_AUDIT:-0}" == "1" ]]; then
  [[ -n "${ETH_RPC_URL:-}" ]] || { echo "S1 MIA selector mint audit is missing. Set ETH_RPC_URL once to collect it: $MIA_MINT_AUDIT" >&2; exit 2; }
  VEGETA_S1_MIA_MINT_AUDIT="$MIA_MINT_AUDIT" \
    bash tools/legacy-scripts/run-vegeta-s1-mia-mint-audit.sh
else
  echo "reusing S1 MIA selector mint audit: $MIA_MINT_AUDIT"
fi

# Validate/build the reusable base families plus the reviewed S1 drop/STG extensions.
bash tools/legacy-scripts/run-vegeta-s1-native-implementation-validation.sh "$PLAN_DIR"

INITIAL_STATE_MODE="${VEGETA_S1_NATIVE_INITIAL_STATE_MODE:-rpc}"
if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "S1 publication replay needs predecessor-block logical state. Set ETH_RPC_URL (archive-capable) or reuse a populated $EXEC_DIR/evm-initial-state-cache.json." >&2
  exit 2
fi
PREP=(
  --plan "$PLAN_DIR/native-plan.jsonl"
  --selector-map "$PLAN_DIR/selector-semantic-map.json"
  --code-cache "$WORK_DIR/code-cache.json"
  --output-dir "$EXEC_DIR"
  --initial-state-mode "$INITIAL_STATE_MODE"
  --caller-mode exact
  --dataset-label vegeta-s1-native
  --implementation-manifest evaluation/vegeta/s1-native-implementation-manifest.v1.json
  --cw721-drop-mint-sequence "$MINT_SEQUENCE"
  --erc721-selector-mint-audit "$MIA_MINT_AUDIT"
  --readiness-report "$PLAN_DIR/readiness.json"
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP+=(--rpc-url "$ETH_RPC_URL"); fi
python3 tools/vegeta/prepare-native-s3-execution.py "${PREP[@]}"
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

echo
echo "PASS: Vegeta S1 prepared Wasmd workload is ready for the five-system campaign"
echo "plan: $PLAN_DIR/native-plan.jsonl"
echo "execution: $EXEC_DIR/execution-plan.jsonl"
echo "symbolic profiles: benchmarks/symbolic/native-s3 (base registry + reviewed S1 cw721-drop/STG extensions)"
