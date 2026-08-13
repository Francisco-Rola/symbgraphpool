# Next work: evaluation and bottleneck-driven tuning

Bricks 5A–5F and the common harness are implemented. The current priority is accepted release-mode
data, not another executor redesign.

1. Run `./scripts/run-conflictlab-release-suite.sh quick` and inspect the generated plot-ready CSV.
2. Run the `core` ConflictLab matrices: granularity, contention, block scaling and phase changes.
3. Identify break-even transaction complexity, optimal worker count, planner scaling, and where
   cost-aware policy beats probability-only/static.
4. Only then run the policy-tuning and ingress/block matrices from the `full` suite.
5. Add MiniWarehouse to the same manifest/record/acceptance pipeline.
6. Add explicit predicted-block versus produced-block perturbation before claiming robustness to
   mempool/block prediction error.
7. Integrate external workloads only after the two first-party families use the same methodology.

See `evaluation/README.md`, `evaluation/conflictlab/README.md`, and `tuning-knobs.md`.
