# Evaluation manifests and Brick 5F acceptance

Brick 5F separates raw `ExperimentRecord` samples from publication acceptance. A benchmark campaign
first commits an explicit manifest containing every expected run identity and the acceptance policy,
then validates the resulting JSONL file with the common `acg-evaluate` tool.

A run identity is the tuple:

```text
workload + mode + run_index + seed + workers + exact workload parameters
```

This makes missing, unexpected, and duplicate samples machine-detectable.

## Publication acceptance

`AcceptancePolicy::publication()` requires:

- non-empty experiment/workload/mode identity;
- UTC start time, Git revision, build profile, and Rust compiler version;
- `cpu_model`, `git_dirty`, `kernel`, `logical_cores`, and `os` environment metadata;
- explicit workload parameters;
- no execution-worker oversubscription relative to the record or manifest physical-core budget;
- serial-equivalent work, serial-cost DAG, observed-service DAG, service inflation, and scheduler
  realization measurements;
- canonical and serial-reference state digests that match;
- an explicit `serial_equivalent=true` result;
- internally consistent edge/dependency counts and timing totals.

Performance thresholds are optional and live in the manifest. Publication validity alone does not
assume that every workload should achieve a speedup.

## Validate records

```bash
./scripts/validate-experiment-records.sh \
  evaluation/example-manifest.json \
  benchmark-results/my-experiment/records.jsonl \
  benchmark-results/my-experiment/acceptance.json
```

The command exits 0 only when the complete manifest is accepted, 1 for an acceptance failure, and 2
for malformed input/tool errors.

`ExperimentAcceptanceReport` classifies failures as:

- `incomplete` — required provenance or measurements are missing;
- `correctness_failure` — serial equivalence or state digest comparison failed;
- `configuration_error` — invalid core budgets, inconsistent counters, duplicate/unexpected runs,
  or malformed experiment identity;
- `performance_regression` — an explicitly configured performance threshold was violated.

The example manifest is illustrative; benchmark campaigns should generate manifests from committed
experiment matrices rather than editing result files after execution.


## Common benchmark harness

`acg-benchmark-harness` now executes manifest runs directly. Each run prepares an independent serial
reference and requested speculative ablation, checks deterministic setup, fills Brick 5E serial/DAG
references and correctness digests, then feeds the records to the Brick 5F manifest evaluator.

Start with:

```bash
./scripts/run-common-benchmark-harness-diagnostics.sh
./scripts/run-benchmark-manifest.sh evaluation/conflictlab-harness-smoke.json
```

See [`../docs/common-benchmark-harness.md`](../docs/common-benchmark-harness.md).
