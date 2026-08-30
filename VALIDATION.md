# Validation

The canonical repository gate is:

```bash
bash scripts/test-all.sh
```

It performs non-mutating Rust formatting checks, Cargo unit/integration tests and doctests for the
core/runtime/benchmark workspaces, Clippy with warnings denied, Python tooling tests, Cosmos SDK
BlockSTM Go tests, and Wasmd Go tests both with and without the Rust ACG FFI bridge. It also checks
shell/Python syntax and `git diff --check`.

Performance evaluation is intentionally separate from the unit-test gate:

```bash
bash scripts/eval-wasmd-debug.sh
bash scripts/eval-wasmd-paper.sh
```

Publication runs should use a clean frozen commit and preserve `records.jsonl`, `environment.txt`,
compute-weight metadata, summary CSV/JSON/text and the records SHA-256 file.
