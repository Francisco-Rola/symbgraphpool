# EuroSys evaluation runner

This directory contains the current cluster runners, postprocessing, plotting, provenance capture, and result validation for Beacon.

## Preflight and smoke

```bash
bash evaluation/eurosys/cluster-preflight.sh
PAPER_EVAL_SMOKE_RESET=1 bash evaluation/eurosys/run-cluster-smoke.sh
```

The smoke run exercises S1, S4, S3/exact-oracle, the zero-conflict path, and semantic correctness with one sample.

## Full cluster campaign

```bash
bash evaluation/eurosys/run-cluster-paper.sh
```

The cluster runner is resume-safe and defaults to three samples. It detects the physical cores available through the process affinity, selects publication worker counts `1,2,4,8,...,max`, runs experiments `00` through `17`, postprocesses the result tree, and performs an integrity scan.

Important overrides:

```text
PAPER_EVAL_MACHINE_TAG
PAPER_EVAL_RESULT_ROOT
PAPER_EVAL_WORKERS
PAPER_EVAL_FEATURE_WORKERS
PAPER_EVAL_SAMPLES
PAPER_EVAL_GRID_SAMPLES
PAPER_EVAL_COMPUTE_SAMPLES
PAPER_EVAL_IAVL_SAMPLES
PAPER_EVAL_CONSENSUS_WINDOW_MS
PAPER_EVAL_MIN_FREE_GB
```

Completed stages are recorded under `<result-root>/.cluster-paper/`; rerunning the same command skips completed experiments unless `PAPER_EVAL_FORCE_RERUN=1` is set.

## Run one experiment

All numbered scripts can be run directly after exporting a common result root and worker/sample configuration. For example:

```bash
export PAPER_EVAL_PROFILE=paper
export PAPER_EVAL_RESULT_ROOT="$PWD/benchmark-results/eurosys/cosmos-paper-3s"
export PAPER_EVAL_WORKERS="1,2,4,8,16,20"
export PAPER_EVAL_FEATURE_WORKERS=6
export PAPER_EVAL_SAMPLES=3
export PAPER_EVAL_GRID_SAMPLES=3
export PAPER_EVAL_COMPUTE_SAMPLES=3
export PAPER_EVAL_IAVL_SAMPLES=3
export PAPER_EVAL_RESOURCE_ACCOUNTING=1
export PAPER_EVAL_REQUIRE_S4=1

bash evaluation/experiments/01_s1_headline.sh
```

## Postprocessing

```bash
PAPER_EVAL_RESULT_ROOT=/path/to/result-root \
  bash evaluation/eurosys/postprocess.sh
```

The postprocessor writes six main PDF figures, supplementary figures when their inputs exist, and two detailed CSV tables under `<result-root>/paper/`. It does not contain manuscript source or manuscript-specific generated files.

The main figures are:

```text
fig01-real-workload-headline.pdf
fig02-scalability-and-tail-distribution.pdf
fig03-generality-contention-ceiling.pdf
fig04-cost-and-overheads.pdf
fig05-prediction-and-adaptation.pdf
fig06-consensus-robustness.pdf
```

The machine-readable tables are:

```text
table1-workloads-fidelity.csv
table2-semantics-correctness.csv
```

Figure 1 uses the full publication width. Figures 2--6 and supplementary plots use the narrower stacked layout implemented by `plot_main.py`.

## Review bundle

```bash
PAPER_EVAL_RESULT_ROOT=/path/to/result-root \
  evaluation/eurosys/make-review-bundle.sh eurosys-cluster-review.zip
```

The bundle contains result CSV/JSON/JSONL/TXT/PDF files, plotting and experiment code, machine metadata, allocation metadata, and Git provenance.
