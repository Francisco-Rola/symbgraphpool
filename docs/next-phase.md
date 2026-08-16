# Next work

1. Run `./scripts/run-conflictlab-parallelism-evaluation.sh` and identify the gap between the
   six-worker hindsight oracle, raw executor, phase bottleneck and full adaptive wall time.
2. Repeat the focused ceiling experiment on native Linux before treating WSL2 numbers as final.
3. Fix the largest measured overhead rather than adding new policy knobs blindly.
4. Add core-count and memory/RSS scaling.
5. Move MiniWarehouse and then external workloads/baselines onto the same harness.
6. Freeze one clean revision for the final paper/artifact evaluation.
