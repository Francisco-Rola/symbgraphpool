# Contributing

1. Keep analyzer-specific representations outside `acg-core`.
2. Never persist dense `ProfileId` values as global identity.
3. Preserve unknown analyzer fields by maintaining forward-compatible serde models where safe.
4. Add a fixture and normalization test for every supported analyzer schema variant.
5. Keep online graph loading deterministic and free of source parsing or symbolic analysis.
6. Run `cargo fmt`, `cargo test --workspace`, and clippy before submitting changes.
