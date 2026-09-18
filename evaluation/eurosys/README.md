# EuroSys publication evaluation

This directory is the publication layer over `evaluation/experiments/`.  It does not replace the existing mechanism experiments; it packages them into six main figures and two tables and adds the reviewer-facing measurements that were previously missing.

## One-command campaigns

Development machine (auto-detects the six physical cores and uses the debug profile by default):

```bash
PAPER_EVAL_MACHINE_TAG=local-6c \
PAPER_EVAL_PROFILE=debug \
bash evaluation/eurosys/run.sh
```

Full local stress pass before moving machines:

```bash
PAPER_EVAL_MACHINE_TAG=local-6c-paper \
PAPER_EVAL_PROFILE=paper \
bash evaluation/eurosys/run.sh
```

Run the same command on the publication machine.  `paper` automatically uses powers-of-two workers up to the detected physical-core count plus the exact maximum, so a 32-core machine produces `1,2,4,8,16,32` without changing scripts:

```bash
PAPER_EVAL_MACHINE_TAG=vegeta-class \
PAPER_EVAL_PROFILE=paper \
bash evaluation/eurosys/run.sh
```

Each machine gets an independent result root under `benchmark-results/eurosys/<machine-tag>/` and a `machine.json` containing CPU, memory, kernel, toolchain, Git hash, and evaluation environment.  Never merge raw records from different machine tags.

`PAPER_EVAL_RESOURCE_ACCOUNTING=1` is enabled by the EuroSys runner.  Each isolated Wasmd strategy is wrapped with GNU `time -v`; the publication plot uses only peak RSS from this process-level measurement.  CPU efficiency is taken from evaluator-internal worker utilization/idle counters so setup/priming time is not mislabeled as scheduler work.

## New collection experiments

`15_compute_sensitivity.sh` runs S1 and S4 at one fixed worker count with compute scales `0,1,2,4,8`.  It pins one evaluator calibration for the whole sweep.  Publication defaults use a 1,000-block prefix and three samples; this is a sensitivity study, not a second 5,000-block headline campaign.  Override with `PAPER_EVAL_COMPUTE_BLOCKS`, `PAPER_EVAL_COMPUTE_SAMPLES`, `PAPER_EVAL_COMPUTE_SCALES`, or `PAPER_EVAL_COMPUTE_DATASETS`.

`16_translation_fidelity.sh` reuses the exact S3 source traces and prepared `native-accesses.jsonl`.  It emits source/native conflict precision/recall, critical-path and hot-key-chain fidelity, and source gas/opcode versus native-cost correlation.  It does not benchmark the scheduler again.

`17_iavl_sensitivity.sh` compares the isolated/pruning state-engine configuration with a cached/non-sync-pruning configuration on an S1 prefix.  It is supplementary and runs automatically only under the `paper` EuroSys profile; set `PAPER_EVAL_RUN_IAVL_SENSITIVITY=0` to skip or `=1` to force it.

`04_native_apps.sh` now expands MiniWarehouse from two hotness points to a paper-profile sweep of `0,25,50,75,90,99%`, while smoke/debug retain smaller grids.  Override with `PAPER_EVAL_NATIVE_HOTNESS_BPS`.

## Six main figures

| Figure | Inputs | Intended claim |
|---|---|---|
| `fig01-real-workload-headline.pdf` | S1/S4 headline + canonical overlap metrics | Replay-only speedup is separated from Tail(C) and Commit(C); Vegeta is shown beside the system on the overlap-aware panels. |
| `fig02-scalability-and-tail-distribution.pdf` | S1/S4 raw records | Worker-count tradeoff plus per-block Tail(C) CDFs. Multi-sample campaigns use the median Tail(C) for each block rather than selecting sample 0. |
| `fig03-generality-contention-ceiling.pdf` | native apps, contention sweep, zero-conflict upper bound | MiniWarehouse shows Tail(C) under application contention; controlled ConflictLab contention compares modeled Commit(C) across all systems; the machine ceiling is read from the dedicated upper-bound report. |
| `fig04-cost-and-overheads.pdf` | S3 phase records, S1/S4 economics, peak RSS, block-size sweep | Exclusive phase costs (without double-counting validation/replay inside reconciliation), local elapsed-work amplification, memory, and Tail(C) block-size sensitivity. |
| `fig05-prediction-and-adaptation.pdf` | S3 exact oracle + ConflictLab prediction/adaptation | Residual post-order oracle headroom, measured prediction precision only, hidden-key recovery, and cost-aware workload-regime adaptation. |
| `fig06-consensus-robustness.pdf` | S1 ordering-window postprocessing + divergence grid | Ordering-window sensitivity plus four representative candidate/final divergence cases under the cost-aware policy. |

Two supplementary figures are produced when data is available: compute-intensity sensitivity and real-workload cold-start/adaptation trajectories.

## Two main tables

`table1-workloads-fidelity.csv` retains the detailed workload/fidelity audit, while `table1-workloads-fidelity.tex` is a compact single-column workload index suitable for the paper. Translation precision/recall and cost-correlation results should be discussed in prose rather than compressed into a wide table.

`table2-semantics-correctness.csv` retains every accepted ConflictLab configuration. `table2-semantics-correctness.tex` collapses those configurations into seven semantic classes and reports the number of checked runs, whether replay or a candidate miss was exercised, and serial-state correctness. The acceptance total remains explicit in the caption.

## Postprocess without rerunning

After copying an existing result tree or changing only plotting code:

```bash
PAPER_EVAL_RESULT_ROOT=/path/to/result-root \
bash evaluation/eurosys/postprocess.sh
```

The postprocessor derives per-block metrics, reuse/reexecution/attempt-amplification economics, win/loss summaries, cold-start trajectories, six main figures, and two tables from the raw records.  Keep `records.jsonl`, `raw/`, and `machine.json` in the artifact.

## Publication protocol

The 5,000-block S1/S4 headline remains five samples under `PAPER_EVAL_PROFILE=paper`.  Sensitivity experiments may use explicitly documented prefixes because they test robustness of a parameter rather than estimate the headline effect.  Run the final campaign from a clean Git tree, pin the machine governor/NUMA policy externally if required by the publication host, and archive each machine-tag result tree verbatim.


## Plot interpretation notes

The paper-facing plots use `Ours` rather than the internal `Rust-ACG` strategy label. Line styles and markers are intentionally distinct so the figures remain readable in grayscale. A local campaign with one repetition per configuration is annotated as such and suppresses meaningless zero-width confidence intervals; the annotation disappears automatically once the final multi-sample campaign is postprocessed.

The main S1/S4 campaign currently uses compute scale 4. The compute-sensitivity supplement reports Tail(C) across the configured scale sweep and should be cited when explaining why the design point is not dependent on one synthetic compute setting. S1/S4 translated workloads remain scheduler/contention studies on native Wasmd, not EVM performance reproductions.
