# Validation

The repository correctness gate is:

```bash
bash scripts/test-all.sh
```

It covers Rust formatting/tests/Clippy, Python tooling, Go scheduler tests, the Rust ACG FFI bridge, and shell/Python syntax checks.

Paper experiments are intentionally separate. Run them from a frozen commit via `evaluation/experiments/`; preserve raw JSONL, workload manifests, evaluator/calibration metadata and summarized CSV/JSON with each result directory. See `evaluation/README.md`.
