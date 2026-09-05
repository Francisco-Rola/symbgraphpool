# EuroSys / OSDI evaluation plan

This file is the paper-facing contract for the evaluation. Every experiment below answers a distinct claim; development/debug sweeps are intentionally excluded.

## Claims and evidence

| ID | Claim | Workload | Experiment | Paper output |
|---|---|---|---|---|
| C1 | ACG moves execution off the ordering-critical tail and benefits when pre-consensus work overlaps the consensus window. | Vegeta S1; S4 when ready | `01_s1_headline.sh`, `02_s4_headline.sh` | Fig. 1 replay-x plus overlap-aware tail/commit sensitivity vs workers and consensus window |
| C2 | The result is not an artifact of one translated trace. | MiniWarehouse uniform/hot; native token/NFT/AMM mix | `04_native_apps.sh` | Fig. 2 native replay scaling |
| C3 | The shared Wasmd/WasmVM substrate can exploit the machine when dependencies disappear. | Native ConflictLab, unique key/tx | `05_conflictlab_upper_bound.sh` | Fig. 3 own-1w scaling and efficiency |
| C4 | ACG and baselines degrade predictably as true contention rises. | ConflictLab lane sweep | `06_conflictlab_contention.sh` | Fig. 4 replay-x vs contention |
| C5 | The S1-derived Wasmd workload exposes enough parallelism for conventional schedulers; remaining baseline gaps can be separated into workload/schedule/executor effects without claiming EVM-equivalent reproduction. | S1-derived Wasmd | `01_s1_headline.sh` diagnostics | Fig. 4b translated workload/schedule parallelism |
| C6 | Planning, preexecution, indexed validation and selective replay have distinct costs and the post phase is small. | Vegeta S3 (101 blocks) | `03_s3_breakdown.sh` | Fig. 5 phase breakdown + exact-oracle headroom |
| C7 | MVCC visibility, indexed reconciliation, and profile-indexed feedback each remove measurable overhead. | S3 | `09_s3_acg_ablation.sh` | Fig. 6 implementation ablation |
| C8 | Symbolic precision and adaptive feedback trade prediction precision for graph freedom safely. | ConflictLab | `07_conflictlab_prediction.sh` | Fig. 8 precision/replay tradeoff |
| C9 | Feedback and regime detection recover after prediction misses/workload changes. | ConflictLab transitions/hidden-key faults | `07_conflictlab_prediction.sh`, `08_conflictlab_adaptation.sh` | Fig. 9 recovery trajectory / replay cost |
| C10 | Fixed planning/speculation overhead has an explicit block-size break-even point. | ConflictLab independent-key blocks | `10_conflictlab_block_size.sh` | Fig. 7 speedup vs tx/block |
| C11 | Candidate/decided divergence and finite consensus cutoffs reduce reuse gracefully without violating correctness. | ConflictLab | `11_conflictlab_consensus.sh` | Fig. 10 cutoff/divergence sensitivity |
| C12 | Equivalence-group compaction and transitive reduction keep planning scalable. | ConflictLab | `13_conflictlab_compaction.sh` | Fig. 11 planning time + edge compression |
| C13 | Non-point Wasmd semantics remain correct under speculation. | range reads/deletes, bank/funds, instantiate, state-derived operations | `12_conflictlab_semantics.sh` | `table-semantics.csv` correctness/coverage table (supplement if space) |

## System features exercised

The implementation contains four groups of mechanisms; no feature should be claimed without an experiment above.

**Prediction and graph construction.** Offline symbolic profiles and stable profile IDs, input-dependent predicates, fine/resource/profile symbolic granularity, Unknown-conservative materialization, profile/instance bucketing, equivalence-group compaction, exact transitive reduction, and risk/cost-aware Low/Soft/Hard scheduling. Exercised by C4, C8, C12. The ConflictLab feature matrices are ACG-only mechanism experiments; baseline comparisons remain in S1/S4/native/upper-bound/contention/block-size experiments.

**Pre-consensus execution.** Detached speculative receipts, dependency-driven READY-DAG execution, block-local MVCC visibility, cost-aware dispatch and per-worker execution. Exercised by C1, C3, C6, C7.

**Reconciliation and safety.** Key-indexed canonical validation, candidate-miss detection, selective replay, serialization/fan-out cost learning, and correctness against the serial commit oracle. Exercised by C1, C6-C9, C13.

**Adaptation/control plane.** Decayed probability/confidence, maturity, runtime fallback relationships, exploration, serial bypass/admission economics, regime-change detection/probation, consensus cutoff and candidate-vs-decided divergence. Exercised by C8-C11.

## Workloads

### Vegeta S1
Historical Ethereum transactions from Vegeta S1 translated into the native Wasmd execution substrate. The paper calls this the **S1-derived Wasmd workload** and does not claim EVM execution equivalence. We retain Vegeta's published full-S1 dependency-chain ratio (8.39×) only as provenance/context; all measured structural bounds come from the actual Wasmd accesses executed by this artifact. The headline campaign is 5,000 blocks; `debug` uses a 300-block prefix.

### Vegeta S3
101 blocks. Use only for exact tracing, phase breakdown, oracle/headroom analysis and implementation ablations. It is intentionally not the headline throughput workload.

### Vegeta S4
5,000 later Ethereum blocks (18,581,726--18,586,725). Source collection uses the scalable public-RPC prestate/diff + callTracer/code path and deliberately skips exact per-transaction SLOAD/SSTORE collection. **Native translation is currently a placeholder**; `02_s4_headline.sh` skips cleanly until `s4/native-execution` exists.

### Native CosmWasm applications
`MiniWarehouse` is TPC-C-inspired but not TPC-C compliant. Run a partitioned/uniform setting and a hot-warehouse setting. `NativeMix` combines the repository's controlled CW20, mintable CW721 and Astroport-like pair contracts to give account-local, ownership-index and hot-pool state in one native workload.

### ConflictLab
ConflictLab is not used as a substitute for real workloads. It is the controlled microscope: zero-conflict upper bound, contention/lane sweep, block-size break-even, symbolic granularity, prediction faults, workload transitions, consensus divergence/cutoff, semantic coverage, and graph compaction.

## Primary metrics

Use three complementary views rather than collapsing the architecture into one throughput number.

**Replay speedup (prior-work compatibility).**

`replay_tps = transactions / sum(R)`

`replay_x = replay_tps / Serial replay_tps`

where `R = post_consensus_nanos`. This follows the Vegeta NSDI'25 single-node replay convention and remains the direct baseline-comparison metric. Pre-consensus speculation/planning is excluded. Intrinsic validation/fallback/re-execution is included; harness-only historical-state restoration remains separate.

**Overlap-aware execution tail (architecture-facing metric).** For a consensus window `C` supplied externally to the single-node harness and pre-consensus phase `P = pre_consensus_nanos`, define per block:

`tail(C) = R + max(0, P - C)`

`tail_x(C) = Serial_R / tail(C)`

Serial/BlockSTM/Aria have `P=0`; Vegeta/ACG receive credit only for work that actually fits under `C`. At `C=0`, all prework is charged. As `C` grows, the metric converges to replay-x. This is the preferred single-node metric for explaining whether extra workers are useful because they increase the probability that prework completes before ordering.

**Proposal-to-commit sensitivity.**

`commit(C) = max(C, P) + R = C + tail(C)`

`commit_x(C) = Serial_commit(C) / strategy_commit(C)`

This includes the common consensus interval and therefore shows when consensus itself dominates end-to-end latency. It is a model until a distributed experiment supplies measured per-block consensus durations. Never derive `C` from the evaluated strategy's own pre-consensus time. Normal paper experiments freeze `C=300 ms`; only experiment 14 sweeps the window to establish sensitivity around that independently selected design point.

At the canonical `C=300 ms`, also report `coverage(C)=Pr[P<=C]`, hidden-prework fraction and prework overrun. Experiment 14 alone reports the full S1 sweep, grid break-even against the best baseline, and worker count minimizing modeled commit latency as `C` varies. These diagnostics explain cases where replay-x gets worse with more workers while overlap improves.

Report `reexec-%` as a diagnostic. Strategy-specific raw phase timers remain in JSONL even when omitted from the main table.

For ConflictLab upper-bound scaling, also report each strategy relative to its own 1-worker throughput. For Rust-ACG, use preexecution scaling rather than replay-x as the hardware-scaling diagnostic because useful work intentionally moves before consensus.

## Statistical protocol

`PAPER_EVAL_PROFILE=paper` uses five samples unless an experiment has a stronger fixed seed set. Plot means with 95% confidence intervals where multiple samples are present. Pin the machine, OS/kernel, Go/Rust/CosmWasm versions, worker counts, calibration value and git revision in the artifact metadata. Do not mix results across evaluator hashes or IAVL settings.

Before final numbers, rerun the zero-conflict experiment on native Linux on the same hardware used for the final paper. WSL results are useful diagnostics, not the final causal claim about host overhead.

## Suggested paper layout

Main paper: Fig. 1 S1/S4 replay plus fixed-300ms overlap-aware headline; one separate S1 consensus-window sensitivity figure validates the 300ms design point; Fig. 2 native workloads; Fig. 3 zero-conflict ceiling; Fig. 4 contention plus the S1-derived workload-parallelism diagnostic; Fig. 5 S3 phases/oracle headroom; Fig. 6 implementation ablation; Fig. 7 block-size break-even; Fig. 8 prediction precision; Fig. 9 adaptation; Fig. 10 consensus divergence; Fig. 11 compaction if space. Put semantic coverage, detailed baseline-fidelity counters and secondary phase tables in the appendix/supplement if page pressure is high. No original-EVM executor is required by this artifact.
