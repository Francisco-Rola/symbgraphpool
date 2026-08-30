# Contributing

1. Keep analyzer-specific representations outside `acg-core`.
2. Never persist dense `ProfileId` values as global identity.
3. Preserve unknown analyzer fields with forward-compatible serde models where safe.
4. Add fixtures/normalization tests for supported analyzer schema variants.
5. Keep online graph loading deterministic and free of source parsing or symbolic analysis.
6. Keep transaction scheduling in Rust `acg-*`; Go/Wasmd should execute the emitted plan and enforce
   concrete correctness rather than reimplement symbolic scheduling.
7. Add or update unit tests for graph compaction, scheduling, feedback, reconciliation and FFI
   semantics when changing those paths.

Before accepting a patch, run:

```bash
bash scripts/test-all.sh
```

Long performance campaigns are not part of the normal patch gate. Use the current controlled Wasmd
evaluator in `evaluation/wasmd/README.md`; historical one-off wrappers are under `tools/legacy-scripts/`.
