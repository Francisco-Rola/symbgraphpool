# Evaluation artifact

This directory is the canonical entry point for paper experiments. Historical development scripts outside this tree are implementation helpers, not artifact entry points.

## Prerequisites

The artifact expects the repository Rust/Go toolchains, Python 3, and `matplotlib` for figure generation. Final paper runs should use native Linux on the paper machine; WSL remains useful for development diagnostics but should not be the sole host-overhead claim.

## Quick start

```bash
# Fast sanity run (small prefixes / one sample)
PAPER_EVAL_PROFILE=smoke bash evaluation/experiments/run-all.sh

# Normal local development run
PAPER_EVAL_PROFILE=debug bash evaluation/experiments/run-all.sh

# Publication run: full S1/S4 and five samples
PAPER_EVAL_PROFILE=paper bash evaluation/experiments/run-all.sh
```

Run one experiment by invoking its numbered script directly. Results go to `benchmark-results/paper-eval/` by default; set `PAPER_EVAL_RESULT_ROOT` to change it. Figures and the semantic-coverage CSV go to `evaluation/figures/`; regenerate them with:

```bash
python3 evaluation/plots/plot_all.py
```

`02_s4_headline.sh` skips until the reviewed S4 native execution bundle exists. The S4 post-collection pipeline lives under `tools/vegeta/`: characterize the frozen caches, review/freeze the S4 family map, then run `evaluation/workloads/prepare_s4.sh`. Set `PAPER_EVAL_REQUIRE_S4=1` in the final artifact to make a missing bundle a hard failure.

## Workload preparation

```bash
ETH_RPC_URL=... bash evaluation/workloads/collect_s1.sh
bash evaluation/workloads/prepare_s1.sh
bash evaluation/workloads/prepare_s3.sh
ETH_RPC_URL=... bash evaluation/workloads/collect_s4.sh
bash tools/vegeta/run-vegeta-s4-characterize.sh  # local-only: frozen caches -> family/selector review queue
bash evaluation/workloads/prepare_s4.sh          # requires reviewed evaluation/vegeta/s4-native-family-map.v1.json
```

Native application and ConflictLab inputs are generated deterministically by the experiment scripts and need no network access.

## Experiment map

- `01_s1_headline.sh` -- S1-derived Wasmd headline, all five systems, with translated-workload parallelism bounds.
- `02_s4_headline.sh` -- S4-derived Wasmd headline after the reviewed S4 native bundle is prepared.
- `03_s3_breakdown.sh` -- 101-block phase/oracle dataset.
- `04_native_apps.sh` -- MiniWarehouse uniform/hot and native CW20/CW721/AMM mix.
- `05_conflictlab_upper_bound.sh` -- zero-conflict native Wasm upper bound.
- `06_conflictlab_contention.sh` -- controlled conflict lanes, all five systems.
- `07_conflictlab_prediction.sh` -- symbolic granularity and hidden-key prediction faults.
- `08_conflictlab_adaptation.sh` -- workload-transition feedback/regime adaptation.
- `09_s3_acg_ablation.sh` -- ACG implementation ablation.
- `10_conflictlab_block_size.sh` -- block-size/break-even sweep.
- `11_conflictlab_consensus.sh` -- cutoff and candidate/decided divergence.
- `12_conflictlab_semantics.sh` -- range/bank/instantiate/stateful correctness coverage.
- `13_conflictlab_compaction.sh` -- graph compaction/transitive-reduction scalability.
- `14_consensus_window_sensitivity.sh` -- one S1-only postprocessing sweep used to justify the canonical consensus window; it does not rerun execution.

See [`PAPER_PLAN.md`](PAPER_PLAN.md) for the claim-to-figure mapping and metric definitions. `evaluation/figures/INDEX.txt` is regenerated from whichever result sets are currently present; missing S4 simply means its figure is absent until the workload is ready.

## Consensus-overlap reporting

Normal paper experiments use one canonical external consensus window:

```text
C = 300 ms
```

Override only when reproducing an explicitly different design point:

```bash
PAPER_EVAL_CONSENSUS_WINDOW_MS=300 bash evaluation/experiments/01_s1_headline.sh
```

The single-node harness does not measure consensus latency. `300 ms` is a fixed low-latency WAN BFT design point chosen independently of ACG/Vegeta timings; it is not derived from a strategy's pre-consensus interval. For every strategy the artifact reports:

```text
tail(C)   = R + max(0, P-C)
tail-x(C) = Serial_R / tail(C)
commit(C) = max(C,P) + R
commit-x(C) = Serial_commit(C) / commit(C)
```

where `P` is eligible pre-consensus work and `R` is intrinsic post-consensus work. `replay-x` remains the Vegeta-comparable metric. The fixed-window headline additionally reports pre-consensus completion coverage and overrun.

Only `14_consensus_window_sensitivity.sh` sweeps `C`. It re-summarizes the already collected S1 per-block `P/R` records and therefore does not multiply benchmark runtime. Its default grid is `0,50,100,150,200,250,300,400,500,750,1000 ms`; override with `PAPER_EVAL_CONSENSUS_SWEEP_MS`. Use that one figure to demonstrate that conclusions are not an artifact of the 300 ms choice.

## Reproducibility rules

Do not compare runs with different evaluator hashes, calibration values, IAVL settings or workload manifests. Each Wasmd campaign uses one calibration for every strategy in that campaign and verifies every non-Serial result against a Serial CommitID oracle. Keep the raw JSONL alongside summarized CSV/PDF outputs; the raw records contain the phase and safety counters needed for artifact review.

## Scope boundary

The artifact does **not** implement or claim an original-EVM reproduction of Vegeta S1/S4. S1/S4 provide real transaction provenance, but the evaluated workloads execute native Wasmd translations. The paper should use the names *S1-derived Wasmd* and *S4-derived Wasmd* and report the workload-parallelism diagnostics generated from the actual translated accesses. Vegeta's published S1 chain ratio is context only, not an asserted equivalence target.
