#!/usr/bin/env bash
set -euo pipefail

PHYSICAL_CORES="${ACG_PHYSICAL_CORES:-6}"
if (( PHYSICAL_CORES > 6 )); then
  echo "ACG_PHYSICAL_CORES must not exceed the six-core reference budget" >&2
  exit 2
fi
if (( PHYSICAL_CORES < 2 )); then
  echo "Brick 5E runtime measurement test requires at least two physical cores" >&2
  exit 2
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${ACG_BRICK5E_OUT_DIR:-benchmark-results/brick5e-measurement/${STAMP}}"
mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/summary.txt"
RECORDS="$OUT_DIR/records.jsonl"
: > "$SUMMARY"
: > "$RECORDS"

run_test() {
  local name="$1"
  shift
  local log="$OUT_DIR/${name}.log"
  echo "=== ${name} ===" | tee -a "$SUMMARY"
  set +e
  "$@" 2>&1 | tee "$log"
  local status=${PIPESTATUS[0]}
  set -e
  if (( status != 0 )); then
    echo "FAILED: ${name} (status ${status})" | tee -a "$SUMMARY" >&2
    tail -n 100 "$log" >&2
    exit "$status"
  fi
  grep -E 'test result:|BRICK5E_RECORD_JSON=' "$log" | tee -a "$SUMMARY" || true
  grep -F 'BRICK5E_RECORD_JSON=' "$log" \
    | sed 's/^.*BRICK5E_RECORD_JSON=//' \
    >> "$RECORDS" || true
  echo | tee -a "$SUMMARY"
}

run_test feedback-serialization-state \
  cargo test --release -p acg-feedback serialization_cost -- --nocapture --test-threads=1

run_test candidate-learned-serialization-policy \
  cargo test --release -p acg-candidate-graph learned_serialization_cost_changes_risk \
    -- --nocapture --test-threads=1

run_test experiment-schema \
  cargo test --release --manifest-path runtime/Cargo.toml \
    -p acg-evaluation --test schema -- --nocapture --test-threads=1

run_test runtime-learned-cost-and-record \
  cargo test --release --manifest-path runtime/Cargo.toml \
    -p acg-evaluation --test brick5e_runtime -- --nocapture --test-threads=1

if [[ ! -s "$RECORDS" ]]; then
  echo "FAILED: Brick 5E runtime test did not emit a machine-readable experiment record" >&2
  exit 3
fi

python3 - "$RECORDS" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    records = [json.loads(line) for line in handle if line.strip()]
if not records:
    raise SystemExit("no records")
for record in records:
    if record.get("schema_version") != 1:
        raise SystemExit(f"unexpected schema version: {record.get('schema_version')}")
print(f"validated {len(records)} Brick 5E JSONL record(s)")
PY

echo "Logs: $OUT_DIR"
echo "Summary: $SUMMARY"
echo "JSONL records: $RECORDS"
