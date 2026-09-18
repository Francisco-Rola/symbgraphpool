# Evaluation artifact

This directory contains the canonical evaluation entry points for Beacon. Generated workloads and results are intentionally excluded from Git.

## Cluster workflow

Before a publication run:

```bash
bash evaluation/eurosys/cluster-preflight.sh
PAPER_EVAL_SMOKE_RESET=1 bash evaluation/eurosys/run-cluster-smoke.sh
```

The full resume-safe cluster campaign defaults to three samples and automatically selects an affinity-aware worker sweep:

```bash
bash evaluation/eurosys/run-cluster-paper.sh
```

Run any stage independently by invoking its numbered script under `evaluation/experiments/`. All stages honor `PAPER_EVAL_RESULT_ROOT`, `PAPER_EVAL_WORKERS`, `PAPER_EVAL_FEATURE_WORKERS`, and the sample-count environment variables.

## Frozen external inputs

The final cluster campaign expects the prepared S1, S3, and S4 Wasmd execution bundles plus exact S3 access traces. Synthetic MiniWarehouse, NativeMix, and ConflictLab inputs are generated locally by the experiment drivers.

Minimum transferred data:

```text
benchmarks/corpora/vegeta-ethereum/s1/native-execution/
benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl
benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl
benchmarks/corpora/vegeta-ethereum/s3/native-execution/
benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces/
benchmarks/corpora/vegeta-ethereum/s4/native-plan/readiness.txt
benchmarks/corpora/vegeta-ethereum/s4/native-execution/
```

The source tree also contains preparation utilities under `evaluation/workloads/` and `tools/vegeta/` for reconstructing those frozen inputs when necessary. Characterization caches are preparation artifacts and are not required on the publication machine.

## Experiment map

- `00_validate.sh` -- repository/evaluator validation.
- `01_s1_headline.sh` -- S1-derived Wasmd headline.
- `02_s4_headline.sh` -- S4-derived Wasmd headline.
- `03_s3_breakdown.sh` -- S3 phase and exact-oracle analysis.
- `04_native_apps.sh` -- MiniWarehouse and NativeMix.
- `05_conflictlab_upper_bound.sh` -- zero-conflict scaling ceiling.
- `06_conflictlab_contention.sh` -- controlled contention.
- `07_conflictlab_prediction.sh` -- prediction quality and hidden dependencies.
- `08_conflictlab_adaptation.sh` -- runtime feedback and regime changes.
- `09_s3_acg_ablation.sh` -- implementation ablation.
- `10_conflictlab_block_size.sh` -- block-size sweep.
- `11_conflictlab_consensus.sh` -- candidate/final-order divergence.
- `12_conflictlab_semantics.sh` -- semantic correctness coverage.
- `13_conflictlab_compaction.sh` -- graph compaction scalability.
- `14_consensus_window_sensitivity.sh` -- re-summarizes S1 across ordering windows.
- `15_compute_sensitivity.sh` -- compute-intensity sensitivity.
- `16_translation_fidelity.sh` -- source/native topology and cost fidelity.
- `17_iavl_sensitivity.sh` -- state-backend sensitivity.

## Consensus-overlap reporting

The canonical design point is `C = 300 ms` unless explicitly overridden with `PAPER_EVAL_CONSENSUS_WINDOW_MS`. The evaluator reports:

```text
tail(C)     = R + max(0, P-C)
tail-x(C)   = Serial_R / tail(C)
commit(C)   = max(C,P) + R
commit-x(C) = Serial_commit(C) / commit(C)
```

`P` is eligible pre-consensus work and `R` is consensus-visible post-order work. Canonical fallback required to obtain committed state is charged to `R` while remaining separately visible in diagnostics.
Experiment 14 sweeps the ordering window without rerunning execution; override its grid with `PAPER_EVAL_CONSENSUS_SWEEP_MS`.

## Results and postprocessing

The consolidated plotting path is `evaluation/eurosys/plot_main.py`; the old per-figure plotting stack has been removed. To regenerate figures and machine-readable CSV tables from an existing result tree:

```bash
PAPER_EVAL_RESULT_ROOT=/path/to/result-root \
  bash evaluation/eurosys/postprocess.sh
```

Keep raw `records.jsonl`, `raw/`, `machine.json`, and allocation metadata with the result tree. Do not merge measurements from different Git revisions, evaluator calibrations, worker sets, or machine allocations.

See [`eurosys/README.md`](eurosys/README.md) for cluster-specific details and [`PAPER_PLAN.md`](PAPER_PLAN.md) for the claim-to-experiment mapping.
