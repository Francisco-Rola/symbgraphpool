# Contributing

1. Keep analyzer-specific representations outside `acg-core`.
2. Never persist dense `ProfileId` values as global identity.
3. Preserve unknown analyzer fields by maintaining forward-compatible serde models where safe.
4. Add a fixture and normalization test for every supported analyzer schema variant.
5. Keep online graph loading deterministic and free of source parsing or symbolic analysis.
6. Run `cargo fmt`, `cargo test --workspace`, and clippy before submitting changes.

### Patch validation

Run `./scripts/run-all-tests.sh` before accepting a patch. It is the canonical repository-wide
format/test/Clippy gate; performance/evaluation matrices remain separate because they are long.

When changing ConflictLab semantics, candidate materialization, reconciliation, feedback, admission,
or evaluation telemetry, also run the relevant ConflictLab 1.0 evidence profile documented in
`evaluation/conflictlab/v1-experimental-suite.md`; the full 4,630-record suite remains separate from
the normal per-patch gate because of runtime cost.
