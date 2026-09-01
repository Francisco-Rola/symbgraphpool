#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

WORK_DIR="${VEGETA_S1_NATIVE_WORK_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-characterization}"
PLAN_DIR="${VEGETA_S1_NATIVE_PLAN_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-plan}"
EXEC_DIR="${VEGETA_S1_NATIVE_EXECUTION_DIR:-benchmarks/corpora/vegeta-ethereum/s1/native-execution}"
MINT_SEQUENCE="${VEGETA_S1_CW721_MINT_SEQUENCE:-$WORK_DIR/cw721-drop-mint-sequence.json}"
MIA_MINT_AUDIT="${VEGETA_S1_MIA_MINT_AUDIT:-$WORK_DIR/mia-fd883998-mint-audit.json}"
INITIAL_STATE_MODE="${VEGETA_S1_NATIVE_INITIAL_STATE_MODE:-rpc}"
READINESS="$PLAN_DIR/readiness.json"
IMPL_REPORT="$PLAN_DIR/native-implementation-validation.json"
IMPL_MANIFEST="evaluation/vegeta/s1-native-implementation-manifest.v1.json"

required=(
  "$PLAN_DIR/native-plan.jsonl"
  "$PLAN_DIR/selector-semantic-map.json"
  "$WORK_DIR/code-cache.json"
  "$MINT_SEQUENCE"
  "$MIA_MINT_AUDIT"
  "$READINESS"
  "$IMPL_MANIFEST"
)
for path in "${required[@]}"; do
  [[ -s "$path" ]] || { echo "resume prerequisite missing: $path" >&2; echo "Run tools/legacy-scripts/run-vegeta-s1-prepare-native-scheduler.sh once to regenerate prerequisites." >&2; exit 2; }
done

# Fail closed if the cached native plan predates the planner that defines S1 owner-scoped semantics.
if [[ tools/vegeta/build-native-s3-plan.py -nt "$PLAN_DIR/native-plan.jsonl" ]]; then
  echo "cached native plan is older than tools/vegeta/build-native-s3-plan.py; a full semantic-plan rebuild is required" >&2
  exit 2
fi

python3 - "$READINESS" <<'PY_READINESS'
import json, sys
p=sys.argv[1]; d=json.load(open(p))
if d.get("selected_profile") != "scheduler-fidelity" or not d.get("selected_profile_ready"):
    raise SystemExit(f"cached readiness is not a passing scheduler-fidelity profile: {p}")
print("reusing cached scheduler-fidelity readiness: PASS")
PY_READINESS

# Reuse the expensive Cargo/Wasm validation when it already passed. Set the env var below to force it.
if [[ "${VEGETA_S1_RESUME_REVALIDATE_IMPLEMENTATION:-0}" == "1" ]]; then
  bash tools/legacy-scripts/run-vegeta-s1-native-implementation-validation.sh "$PLAN_DIR"
else
  [[ -s "$IMPL_REPORT" ]] || { echo "cached implementation-validation report missing: $IMPL_REPORT" >&2; echo "Either run the full preparation once or set VEGETA_S1_RESUME_REVALIDATE_IMPLEMENTATION=1." >&2; exit 2; }
  python3 - "$IMPL_REPORT" "$IMPL_MANIFEST" <<'PY_IMPL'
import json, pathlib, sys
report=json.load(open(sys.argv[1])); manifest=json.load(open(sys.argv[2]))
if not report.get("accepted") or not report.get("require_wasm_artifacts"):
    raise SystemExit("cached implementation validation is not an accepted Wasm-artifact validation")
missing=[]
for row in manifest.get("families", []):
    wasm=row.get("wasm_artifact")
    if wasm and not pathlib.Path(wasm).is_file(): missing.append(wasm)
if missing:
    raise SystemExit("cached implementation validation references missing Wasm artifacts: " + ", ".join(missing[:5]))
print("reusing cached native implementation/Wasm validation: PASS")
PY_IMPL
fi

# Seconds-level gate: reconcile reviewed mint adapters against the cached public Transfer audit.
# Reuse an exact audit when it is newer than every input that can change the result.
MINT_RECONCILIATION="$PLAN_DIR/cw721-drop-translation-audit.json"
MINT_RECONCILIATION_INPUTS=(
  "$PLAN_DIR/native-plan.jsonl"
  "$PLAN_DIR/selector-semantic-map.json"
  "$WORK_DIR/code-cache.json"
  "$MINT_SEQUENCE"
  tools/vegeta/audit-vegeta-s1-cw721-drop-translation.py
  tools/vegeta/prepare-native-s3-execution.py
)
reuse_mint_reconciliation=0
if [[ -s "$MINT_RECONCILIATION" ]]; then
  reuse_mint_reconciliation=1
  for path in "${MINT_RECONCILIATION_INPUTS[@]}"; do
    if [[ ! -e "$path" || "$path" -nt "$MINT_RECONCILIATION" ]]; then
      reuse_mint_reconciliation=0
      break
    fi
  done
fi
if [[ "$reuse_mint_reconciliation" == "1" ]]; then
  echo "reusing cached cw721-drop reconciliation audit"
else
  bash tools/legacy-scripts/run-vegeta-s1-cw721-drop-translation-audit.sh
fi
python3 - "$MINT_RECONCILIATION" <<'PY_MINT'
import json, sys
p=sys.argv[1]
d=json.load(open(p)); s=d.get("summary") or {}
required=(
    "expected_mint_transactions",
    "translated_mint_transactions",
    "exact_transactions",
    "issue_transactions",
    "missing_translated_mint_transactions",
    "quantity_mismatch_transactions",
    "unexpected_translated_mint_transactions",
    "expected_mint_events",
    "translated_mint_quantity",
)
missing=[k for k in required if k not in s]
if missing:
    raise SystemExit(f"cw721-drop reconciliation audit is missing required summary fields {missing}: {p}")
vals={k:int(s[k]) for k in required}
failures=[]
if vals["issue_transactions"] != 0: failures.append(f"issues={vals['issue_transactions']}")
if vals["missing_translated_mint_transactions"] != 0: failures.append(f"missing={vals['missing_translated_mint_transactions']}")
if vals["quantity_mismatch_transactions"] != 0: failures.append(f"quantity_mismatch={vals['quantity_mismatch_transactions']}")
if vals["unexpected_translated_mint_transactions"] != 0: failures.append(f"unexpected={vals['unexpected_translated_mint_transactions']}")
if vals["expected_mint_transactions"] != vals["translated_mint_transactions"]:
    failures.append(f"tx_expected={vals['expected_mint_transactions']} translated={vals['translated_mint_transactions']}")
if vals["exact_transactions"] != vals["expected_mint_transactions"]:
    failures.append(f"exact={vals['exact_transactions']} expected={vals['expected_mint_transactions']}")
if vals["expected_mint_events"] != vals["translated_mint_quantity"]:
    failures.append(f"events_expected={vals['expected_mint_events']} translated_qty={vals['translated_mint_quantity']}")
if failures:
    raise SystemExit("cw721-drop reconciliation still fails: " + "; ".join(failures) + f"; inspect {p}")
print(
    "PASS: cached S1 cw721-drop mint reconciliation is exact "
    f"({vals['exact_transactions']} tx / {vals['expected_mint_events']} mint events)"
)
PY_MINT

if [[ "$INITIAL_STATE_MODE" == "rpc" && -z "${ETH_RPC_URL:-}" && ! -s "$EXEC_DIR/evm-initial-state-cache.json" ]]; then
  echo "RPC-backed predecessor state is not cached. Set ETH_RPC_URL once, or run after a populated $EXEC_DIR/evm-initial-state-cache.json exists." >&2
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
  --implementation-manifest "$IMPL_MANIFEST"
  --cw721-drop-mint-sequence "$MINT_SEQUENCE"
  --erc721-selector-mint-audit "$MIA_MINT_AUDIT"
  --readiness-report "$READINESS"
)
if [[ -n "${ETH_RPC_URL:-}" ]]; then PREP+=(--rpc-url "$ETH_RPC_URL"); fi

# The execution plan must be rebuilt because translation semantics changed, but all earlier expensive
# coverage/readiness/Wasm stages are intentionally reused by this resume wrapper.
python3 tools/vegeta/prepare-native-s3-execution.py "${PREP[@]}"
python3 tools/vegeta/validate-native-s3-execution.py --output-dir "$EXEC_DIR" --prepared-only

echo
echo "PASS: resumed Vegeta S1 scheduler preparation completed"
echo "reused: native plan, readiness, public mint audits, implementation/Wasm validation"
echo "rebuilt: $EXEC_DIR/execution-plan.jsonl and execution-manifest.json"
