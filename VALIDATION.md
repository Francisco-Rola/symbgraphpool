# Validation

The repository correctness gate is:

```bash
bash scripts/test-all.sh
```

It covers Rust formatting/tests/Clippy, Python tooling, Go scheduler tests, the Rust ACG FFI bridge, and shell/Python syntax checks.

Paper experiments are intentionally separate. Run them from a frozen commit via `evaluation/experiments/`; preserve raw JSONL, workload manifests, evaluator/calibration metadata and summarized CSV/JSON with each result directory. See `evaluation/README.md`.

### S4 third conflict-closure batch

After the first and second S4 review batches, install the conservative third closure batch and recompute the exact unique conflict union:

```bash
bash tools/vegeta/run-vegeta-s4-apply-third-batch.sh
cat benchmarks/corpora/vegeta-ethereum/s4/native-characterization/conflict-closure.txt
cat benchmarks/corpora/vegeta-ethereum/s4/native-characterization/family-review-readiness.txt
```

The batch contains only reviewed token-family aliases plus a conservative per-pool V3 swap lock. AMP and bridge/rollup families remain pending. Freeze only when `conflict-closure.txt` reports `family freeze gate: PASS`.
