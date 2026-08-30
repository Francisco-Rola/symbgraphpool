Frozen native-S3 formatting recovery bundle

Why this exists
---------------
The earlier scripts/test-all.sh used `cargo fmt --manifest-path benchmarks/Cargo.toml --all`.
The benchmarks workspace includes the ten native-S3 contracts. Their exact source bytes and
line windows are frozen evidence for benchmarks/symbolic/native-s3/*.symbolic.json, so rustfmt
changed source_sha256 values and invalidated provenance.

How to apply
------------
From the repository root, extract/copy this bundle with overwrite enabled. It contains:
  * the corrected scripts/test-all.sh;
  * two formatting-insensitive Python source-policy tests;
  * the canonical ten native-S3 src/lib.rs files matching checked-in symbolic provenance.

Then run:
  bash scripts/test-all.sh

The corrected test-all script still auto-formats the core and runtime workspaces. In the
benchmarks workspace it auto-formats only the non-frozen conflictlab, miniwarehouse, and
vegeta-trace crates; it still tests and Clippy-checks the full benchmarks workspace.
