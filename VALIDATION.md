# Validation

Use local Cargo/CI as the authoritative compiler gate.

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

cargo fmt --manifest-path runtime/Cargo.toml --all -- --check
cargo test --manifest-path runtime/Cargo.toml --workspace --all-targets
cargo clippy --manifest-path runtime/Cargo.toml --workspace --all-targets -- -D warnings

cargo test --manifest-path benchmarks/Cargo.toml --workspace
```

Focused system/evaluation gates:

```bash
./scripts/run-brick5f-system-acceptance.sh
./scripts/run-common-benchmark-harness-diagnostics.sh
./scripts/run-evaluation-tools-tests.sh
```

Release ConflictLab evaluation builds the real Wasm contract and must be run with release binaries:

```bash
./scripts/run-conflictlab-release-suite.sh quick
```

This generated patch was statically checked in an environment without a Rust toolchain using shell
syntax checks, JSON/TOML parsing, Python utility tests, patch dry-run/application, whitespace checks,
and resulting-tree equivalence. Rust compilation/test/Clippy remains the local gate.

## Common benchmark harness (2026-08-13)

Added `runtime/crates/acg-benchmark-harness`, a manifest-driven workload adapter/runner layered above
Brick 5F. Every run independently prepares a serial reference and requested speculative ablation,
checks deterministic setup, executes warm-up plus one measured block, fills Brick 5E serial/DAG and
correctness fields, writes JSONL, and evaluates the complete dataset through Brick 5F.

Focused acceptance command:

```text
./scripts/run-common-benchmark-harness-diagnostics.sh
```

Generic manifest execution:

```text
./scripts/run-benchmark-manifest.sh <manifest.json> [output-directory]
```

The first built-in ConflictLab adapter supports static, probability-only, and cost-aware modes,
manifest-driven workload/scheduler/feedback knobs, and distinct warm-up versus measured contention
and work-cost parameters. Unknown workload or reserved `acg.*` parameters are rejected rather than
silently ignored.

Focused tests cover deterministic preparation, independent serial/adaptive state, serial-equivalence
digests, serial/DAG reference population, ablation-specific feedback separation, tuning-parameter
parsing, worker-budget enforcement, output-file round trips, and unknown workload handling. The
end-to-end smoke manifest executes all three policy modes and must be accepted by Brick 5F.

Static construction-environment validation completed with TOML/JSON parsing, shell `bash -n`,
lightweight changed-Rust delimiter checks, and patch whitespace/application checks. Definitive Rust
acceptance remains local `cargo fmt`, runtime workspace tests, and runtime workspace Clippy with
`-D warnings`, followed by the focused harness diagnostics runner.

## Full repository gate

After every patch, run:

```bash
./scripts/run-all-tests.sh
```

The gate runs `cargo fmt --all` for the root, runtime and benchmark workspaces, repository diff and
script syntax checks, every Rust workspace/all-target test, doc tests, Clippy with `-D warnings`, and
the evaluation-tool tests. The long ConflictLab matrices are intentionally separate.

## ConflictLab 1.0 submission evidence gate

The full internal submission suite is separate from the normal repository gate because it contains
4,630 measured real-Wasm configurations plus warm-up/history blocks. The suite definition and claim
mapping live in `evaluation/conflictlab/v1-experimental-suite.md`.

Before running it:

```bash
./scripts/run-all-tests.sh
```

For the complete evidence set:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

The runner merges every campaign into one `records.jsonl`, runs Brick-5F/Phase acceptance, executes
`scripts/validate-conflictlab-v1.py`, aggregates CSVs, and writes
`scripts/summarize-conflictlab-v1.py`'s reviewer-facing summary. Reusing the same output directory
resumes the suite: only campaigns whose cached manifest, accepted report, record count, and run
identities exactly match the current grid are reused; incomplete/stale campaign directories are
removed before rerun. A failed campaign no longer prevents later independent campaigns from
running, but the suite still exits non-zero after post-processing until every requested campaign is
valid. All 1.0 grids are fixed to six workers; this initial suite intentionally does not make
core-count or memory-capacity scaling claims.

Evaluation-only ablations (`acg.compact_equivalence_groups`, symbolic granularity, and controlled
prediction faults) must remain out of the production default path. Additive 1.0 telemetry remains
schema-3 compatible through serde defaults so historical records continue to parse.

For a final paper/artifact run, use a clean commit (`git_dirty=false`) on the intended evaluation
machine and preserve `suite-environment.txt`, `campaign-counts.json`, validation output, merged raw
records, and aggregate CSVs together. High-percentile paper claims should come from the dedicated
statistical campaign/repeated clean process runs rather than two-seed coverage matrices.
