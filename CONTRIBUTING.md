# Contributing

Keep symbolic scheduling policy in Rust and concrete execution/correctness enforcement in the runtime/Wasmd layer. Preserve stable profile identities, deterministic graph construction, conservative Unknown semantics, and serial-equivalent commit behavior. Changes to graph construction, feedback, scheduling, MVCC visibility or reconciliation must include focused tests.

Run the repository gate before committing:

```bash
bash scripts/test-all.sh
```

Performance campaigns are not part of the patch gate. Use only the canonical entry points documented in `evaluation/README.md`.
