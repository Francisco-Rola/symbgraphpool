# Next work: control-plane validation, then data-driven tuning

The execution/adaptation bricks and common harness are implemented. Before the large matrices,
validate the dense-graph corrections and force the adaptive loop to make real speculate/serialize
decisions.

1. Run `./scripts/run-control-plane-corrections-diagnostics.sh`.
2. Run `./scripts/run-conflictlab-control-plane-evaluation.sh` and inspect dependency compression,
   feedback batching factors, corrected scheduler realization, soft edges, replays and candidate
   misses.
3. If dense B200 feedback is reduced to low single-digit milliseconds without correctness or
   executor regression, run the larger granularity/contention/phase-change matrices.
4. Tune scheduler/feedback constants only after default-policy release baselines are established.
5. Add MiniWarehouse to the same manifest/record/acceptance pipeline.
6. Add explicit predicted-block versus produced-block perturbation before claiming robustness to
   mempool/block prediction error.
7. Integrate external workloads after both first-party families share the same methodology.

See `evaluation/README.md`, `evaluation/conflictlab/README.md`, and `tuning-knobs.md`.
