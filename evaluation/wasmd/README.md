# Wasmd evaluation metrics

This directory contains the common five-system summarizer and raw-record diagnostics used by the canonical experiments in `evaluation/experiments/`.

The publication-facing single-node metric follows Vegeta's replay convention:

```text
replay-tps = transactions / consensus-visible post phase
replay-x   = replay-tps / Serial replay-tps
```

Pre-consensus speculation/planning is excluded. Intrinsic post-decision validation, fallback and re-execution required by the algorithm are included. Harness-only historical-state restoration for reordered fixed-history traces is recorded separately. `pre_consensus_nanos` is local speculation/planning time, **not measured consensus latency**.

For normal paper runs (one canonical `C`, 300 ms by default), the primary table is:

```text
system  w  n  replay-tps  replay-x  tail-x  commit-x  cover-%  post-ms  reexec-%  reexec-ms
```

`summary.csv`, `summary.json`, and `per-sample.csv` mirror the same canonical overlap fields (`consensus_window_ms`, `overlap_tail_x`, `commit_x`, `pre_coverage_pct`, plus detailed tail/commit/overrun fields). A dedicated multi-window sensitivity summary deliberately leaves those fixed-point columns out of the primary rows because it has no single canonical `C`.

Raw JSONL intentionally retains detailed phase counters, validation timers, baseline-fidelity diagnostics and serial-equivalence metadata for audit/rebuttal use. Do not use removed fixed-C/model speedups as measured throughput.

Use `evaluation/README.md` for runnable commands and `evaluation/PAPER_PLAN.md` for the figure/claim map.

## Overlap-aware reporting

The summarizer retains replay-x for Vegeta comparability and additionally evaluates one canonical external consensus window, `C=300 ms`, for normal paper experiments. With prework `P` and intrinsic post work `R`, `tail(C)=R+max(0,P-C)` and `commit(C)=max(C,P)+R`. The canonical point is reported directly in the main summary table and machine-readable summary rows; `consensus-sweep.csv` retains the more detailed hidden-work/overrun accounting. `C` is not measured by this single-node harness. Only `evaluation/experiments/14_consensus_window_sensitivity.sh` sweeps `C`, reusing the already collected S1 raw records without rerunning execution.
