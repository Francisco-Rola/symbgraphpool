# Wasmd publication evaluation plan

This is the maintained publication-facing evaluation path for Adaptive Conflict Graph (ACG). The
current goal is a controlled systems comparison on one real execution substrate before expanding to
multi-machine consensus integration and additional workloads.

## Systems in the controlled Wasmd matrix

Every row executes the same translated Vegeta S3 transactions, native CosmWasm artifacts, bank
state, Cosmos SDK v0.54.4 keepers, Wasmd v0.70.3/WasmVM runtime, deterministic compute calibration,
and consensus-decided transaction order.

1. **Serial** — direct canonical keeper execution after consensus.
2. **BlockSTM** — Cosmos SDK `txnrunner.NewSTMRunner`; entirely post-consensus.
3. **AriaFB** — same-VM mechanism adaptation of the repository's existing Rust AriaFB baseline:
   fully parallel batch discovery after consensus, Aria Rule-2-like forward fallback, then canonical
   concrete read-set validation/replay. This is not upstream Aria source code.
4. **Vegeta** — same-VM speculate-order-replay adaptation: block-start speculation before consensus,
   then canonical validation/replay after consensus. This preserves the benchmark's decided-order
   state-machine semantics rather than claiming source-code identity with Vegeta.
5. **Rust-ACG** — offline symbolic profile graph + online atomic transaction candidate graph,
   adaptive feedback, risk-bounded Rust scheduler, dependency-ready Wasmd execution, concrete
   canonical validation/replay.

Every measured block must finish with the same full state digest as the matched direct serial run.

## Primary timing model

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

For a full sample:

```text
throughput = total_transactions / sum(evaluation_service_time)
```

The campaign also reports:

- **post-x** = matched serial execution / post-consensus time. This is the Vegeta-style
  consensus-visible speedup and isolates validation/replay from serial execution.
- **wall-x** = matched serial execution / actual measured strategy wall. This is bookkeeping for
  total machine cost and must always be shown beside post-x.
- pre-consensus p50/p95/p99/max and headroom relative to `C`;
- validation time and replay-execution time where exposed by the runner;
- reexecution/replay rate, forward fallbacks for AriaFB, and state equivalence.

The fixed-window throughput metric is an evaluation model, not a claim that consensus itself is
free. Final publication plots should include a sensitivity analysis with externally fixed consensus
windows once the distributed consensus experiment exists.

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
