# Validation

The canonical repository gate is:

```bash
./scripts/run-all-tests.sh
```

It formats the root/runtime/benchmark workspaces, checks shell and Python syntax, runs all Cargo
workspace tests and doctests, runs Clippy with warnings denied, and executes the evaluation-tool
regression suite.

Long performance evaluations are separate:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
./scripts/run-conflictlab-parallelism-evaluation.sh
```

ConflictLab 1.0 is the broad correctness/mechanism suite. The parallelism-ceiling experiment is the
focused six-worker study for oracle-vs-obtained parallelism and overhead attribution.

For final artifact runs, use a clean frozen commit and preserve raw `records.jsonl`, acceptance
reports, environment metadata, summaries and aggregate CSVs.
