# Wasmd publication evaluation plan

This is the maintained publication-facing evaluation path for Adaptive Conflict Graph (ACG). The
current goal is a controlled systems comparison on one real execution substrate before expanding to
multi-machine consensus integration and additional workloads.

## Systems in the controlled Wasmd matrix

Every row executes the same translated Vegeta S3 transaction payloads, native CosmWasm artifacts,
bank state, Cosmos SDK v0.54.4 keepers, Wasmd v0.70.3/WasmVM runtime, and deterministic compute
calibration. Serial, BlockSTM, and Rust-ACG preserve the historical block order. Vegeta is allowed to
choose the proposal order exactly because proposal reordering is part of the upstream design; AriaFB
commits a serialization admitted by Aria Rule 2 and its fallback DAG.

1. **Serial** — direct canonical keeper execution after consensus.
2. **BlockSTM** — Cosmos SDK `txnrunner.NewSTMRunner`; entirely post-consensus.
3. **AriaFB** — same-Wasmd port of the AriaFB path in the attached Vegeta repository: one
   post-consensus Aria batch, exact Rule-2 abort condition (`WAW || (RAW && WAR)`), then the
   repository's transitively reduced fallback DAG with hot-chain prioritization. Cosmos dynamic
   key/range changes retain an additional conservative safety replay. This is a mechanism port to
   Wasmd, not the upstream Ethereum execution engine.
4. **Vegeta** — same-Wasmd port of the paper's Algorithm 1 + replay path: pre-consensus concrete
   access discovery, **all** point-key dependency chains sorted longest-to-shortest, pairwise DAG
   classification with `RAW+WAR -> WAW`, Rule-2-compatible replay batches, and
   `checkR`/`checkW`-style known/new access handling. Cosmos
   iterator ranges use a conservative extension because the Ethereum implementation has no direct
   range-query analogue.
5. **Rust-ACG** — offline symbolic profile graph + online atomic transaction candidate graph,
   adaptive feedback, risk-bounded Rust scheduler, dependency-ready Wasmd execution, concrete
   canonical validation/replay.

Every measured block must finish with the same full state digest as an independently executed serial
reference. Serial, BlockSTM, Rust-ACG, and the full-trace state gate use historical block order. Vegeta
may derive a proposal/serialization order inside a block, but the fixed historical multi-block workload
cannot safely carry a different post-block state into later source transactions. The harness therefore
stages Vegeta's result and falls back to a measured historical-order replay when the derived execution
is semantically invalid or produces different persistent state. `vegeta_canonical_fallback` records
that choice per block. On normal blocks, `matched_serial_nanos` is the serial timing for Vegeta's
derived serialization; on fallback blocks it is the historical replay timing actually committed. The
record field `serial_reference_scope` identifies this mixed derived-serialization + historical-state-gate
adaptation. `historical_serial_nanos` remains the common historical-order timing control.

## Primary timing model

The primary single-node throughput metric now follows Vegeta NSDI'25 Figure 10. The paper amortizes
pre-consensus speculation through pipelining and compares the replay phase against ordinary Serial
execution. We apply that same consensus-visible definition to every system:

```text
replay-throughput = total_transactions / sum(post_consensus_nanos)
replay-x          = replay-throughput / Serial replay-throughput
```

For Serial, `post_consensus_nanos` is ordinary serial execution. For BlockSTM and AriaFB it is their
post-consensus parallel/fallback execution. For Vegeta and Rust-ACG it excludes pre-consensus
speculation/planning. Vegeta's `post_consensus_nanos` contains the paper Algorithm-3 replay,
validation and intrinsic re-execution, but deliberately excludes our additional historical-state gate;
that adaptation is recorded separately as `vegeta_historical_fallback_nanos` and remains charged to
`strategy_total_nanos`. This makes replay throughput match the paper definition while
**active-work-x** (`historical Serial / strategy_total_nanos`) exposes the full cost of our fixed-trace
state-equivalence adaptation.

`post_x` remains a serialization-matched internal diagnostic. For reordered Vegeta it can differ from
the paper-compatible common-Serial `replay-x`; use `historical_post_x` / `throughput_speedup` for the
paper-style cross-system comparison.

### Secondary consensus-overlap model

Let `C` be one fixed consensus window for an entire campaign:

```text
C = max(pre_consensus_nanos)
    over every measured Rust-ACG and Vegeta block,
    worker count, and sample in that campaign.
```

This follows the speculative-before-consensus model while avoiding a different free window for each
system or worker count.

For each block, the evaluation service time is:

```text
Serial / BlockSTM / AriaFB = C + post_consensus
Vegeta / Rust-ACG          = C + post_consensus
```

The difference is what happens inside `C`: Rust-ACG and Vegeta may perform their pre-execution
work during consensus, while Serial, BlockSTM, and AriaFB wait for consensus and begin execution
afterwards. Charging the same `C` to every strategy makes throughput a full block-cycle metric rather
than giving post-consensus-only baselines a zero-duration consensus phase.

For a full sample this secondary model is:

```text
consensus-model-throughput = total_transactions / sum(C + post_consensus)
```

The campaign also reports:

- **replay-x** / `throughput_speedup` = common historical Serial replay throughput / system replay throughput;
- **matched-x** / `post_x` = serialization-matched Serial / post, retained for internal correctness diagnosis;
- **work-x** / `historical_wall_x` = common historical Serial / actual measured active strategy work;
- **model-x** / `consensus_model_speedup` = the secondary fixed-`C` consensus-overlap model;
- pre-consensus p50/p95/p99/max and headroom relative to `C`;
- validation time and replay-execution time where exposed by the runner;
- reexecution/replay rate, AriaFB Rule-2 fallbacks, conservative Wasmd safety replays, and serialization equivalence.

The fixed-window consensus model is not the primary throughput metric and is not a claim that
consensus itself is free. Final publication plots should include a sensitivity analysis with externally
fixed consensus windows once the distributed consensus experiment exists.

### Faithful Vegeta vs Serial check

After changing Vegeta semantics, run the dedicated two-worker check before launching the full matrix:

```bash
# Default: 300-block fidelity/performance gate.
time bash scripts/eval-wasmd-s1-vegeta-paper-compare.sh

# Full S1 paper-method comparison after the prefix passes:
S1_VEGETA_COMPARE_BLOCKS=5000 time bash scripts/eval-wasmd-s1-vegeta-paper-compare.sh
```

The script builds/reuses one exact evaluator, generates a matched Serial oracle, executes Vegeta, then
prints the Vegeta-paper replay throughput, total active-work cost, dependency-chain ratio, and four
separate replay-cost buckets: point-key Algorithm-3 validation, Wasmd-only range validation,
intrinsic Algorithm-3 re-execution, and the historical-state fallback used only by our fixed-trace gate.

## Ready-to-run evaluation stages

### 0. Repository gate

```bash
bash scripts/test-all.sh
```

This runs the Rust workspaces, doctests and Clippy; Python tooling tests; Cosmos BlockSTM Go tests;
and Wasmd Go tests both with and without the Rust ACG bridge.

### 1. Local debug campaign

```bash
bash scripts/eval-wasmd-debug.sh
```

The default is one sample. On a six-physical-core machine it uses 2, 4, and 6 workers, avoiding SMT
as part of the primary scaling curve. Override with `EVAL_WASMD_WORKERS` and
`EVAL_WASMD_SAMPLES`.

### 2. Publication-style single-machine campaign

From a clean committed tree on an otherwise idle host:

```bash
bash scripts/eval-wasmd-paper.sh
```

Paper mode uses powers of two up to the detected physical-core count plus the full physical-core
count, five independent samples, and rejects a dirty tree by default.

The primary output is:

```text
benchmark-results/wasmd-paper/summary/summary.txt
benchmark-results/wasmd-paper/summary/summary.csv
benchmark-results/wasmd-paper/summary/per-sample.csv
benchmark-results/wasmd-paper/records.jsonl
benchmark-results/wasmd-paper/environment.txt
```

## Publication research questions

### RQ1 — Does ACG improve execution-limited throughput?

Plot fixed-window throughput versus physical worker count for all five systems. Report throughput
speedup over serial and absolute transactions/s.

### RQ2 — How much consensus-visible work remains?

Report post-x, post-consensus p50/p95/p99, validation time, replay execution, and replay rate.
Separate ACG and Vegeta's hidden pre-consensus work from post-consensus service.

### RQ3 — Why does ACG outperform or underperform?

Use ACG's candidate-edge count, final DAG edges, critical-path cost, worker utilization, oracle DAG
parallelism, serialization gap, MVCC hit/fallback counters, and planner subphases. This is an
explanatory/ablation result, not a primary baseline comparison.

### RQ4 — Does online feedback improve over static symbolic information?

Run frozen static-only versus adaptive configurations after the main comparison is stable. Measure
post-x, replay, and time-to-adapt across workload regime changes. Do not tune thresholds on the
publication test set.

### RQ5 — What is the overhead of the mechanism?

Break pre-consensus work into request construction, Rust decode/profile resolution, candidate graph,
scheduler, final reduction, speculative execution, and feedback. Report allocation/CPU profiles only
from separate profiling processes so profiling does not contaminate publication timings.

### RQ6 — How sensitive are results to consensus budget and hardware parallelism?

After the main campaign, sweep externally fixed consensus windows and physical-core counts. Treat SMT
(threads beyond physical cores) as a separate experiment rather than mixing it into the primary
scaling curve.

## What is still required before an OSDI/EuroSys submission

The current same-machine Wasmd matrix is a strong controlled mechanism comparison, but it is not yet
a complete publication evaluation. Before submission, add:

- at least one additional real workload family beyond the translated Vegeta S3 trace;
- a dedicated server with enough physical cores for a meaningful scalability curve, with CPU
  affinity, frequency/governor policy, memory, kernel and toolchain recorded;
- randomized or process-isolated strategy order and explicit warm-up policy;
- at least 5–10 independent process samples for headline results, with confidence intervals;
- externally fixed consensus-window sensitivity rather than only the campaign-derived `C`;
- a distributed/leaderless-consensus integration showing that pre-consensus work actually overlaps
  the protocol window;
- upstream/native baseline implementations where feasible, or clearly labelled mechanism
  adaptations when a common Wasmd substrate makes source reuse impossible;
- workload characterization: block size, access-set size, conflict rate, cost skew, contract-family
  mix, and oracle parallelism;
- ablations for static profiles, online feedback, risk scheduling, MVCC visibility, indexed
  validation, graph compaction/reduction, and serial bypass/admission where applicable;
- artifact packaging with raw records, environment files, exact commit, scripts, frozen inputs, and
  deterministic summary generation.

## Figure/table plan

A compact paper evaluation can be organized around:

1. **Main throughput figure:** five systems × worker count, fixed consensus window.
2. **Post-consensus latency figure:** post-x and p95/p99 post latency.
3. **Replay/correctness table:** replay %, validation/replay decomposition, state-equivalence gate.
4. **Scheduler-quality figure:** ACG DAG parallelism versus actual-access oracle and worker utilization.
5. **Overhead breakdown:** planning/preexecution/reconciliation/feedback.
6. **Ablation figure:** static → adaptive → cost/risk aware → current optimized implementation.
7. **Sensitivity:** consensus-window length and contention/conflict regime.

## Baseline references

- Xu et al., *Vegeta: Enabling Parallel Smart Contract Execution in Leaderless Blockchains*, NSDI
  2025. The paper introduces speculate-order-replay and pre-consensus transaction processing.
- Lu et al., *Aria: A Fast and Practical Deterministic OLTP Database*, PVLDB 2020. The repository's
  AriaFB row is a canonical-order mechanism adaptation of the forward-dependency fallback used in
  the existing benchmark harness.
- Gelashvili et al., *Block-STM: Scaling Blockchain Execution by Turning Ordering Curse to a
  Performance Blessing*, 2022. The Wasmd row uses the Cosmos SDK transaction runner implementation
  on the same keeper/Wasm substrate rather than an access-trace simulation.
