# ConflictLab 1.0 submission experimental suite

ConflictLab 1.0 is the frozen internal-evidence suite for the ACG paper. It is intended to answer
mechanism, correctness, robustness, scalability, and reproducibility questions before external
benchmarks or competing baselines are added. The suite does **not** add a cross-block execution
pipeline, scheduler exploration, or a new production policy. Its extra switches are evaluation
ablations of mechanisms already present in ACG.

## Evaluation contract

The current suite fixes execution to the local six-physical-core machine and uses six ACG workers.
Core-count and memory-capacity scaling are deliberately deferred. Block-size scaling remains part of
this suite because it tests candidate-graph/control-plane complexity rather than hardware scale.
All measured ConflictLab campaigns use the real Wasm backend. Canonical performance runs use **benchmark-scoped retained VM reuse** (`vm_instance_lifecycle=reuse`) because CosmWasm 2.0.9 exposes compiled-module caching but not a public API for restoring a dirty `Instance` to its post-instantiation state. Retained reuse is therefore not claimed as a generally safe CosmWasm execution mode. For ConflictLab V1 we make the retained instance's cumulative gas budget non-binding (`vm_gas_limit=u64::MAX`) and require targeted fresh-instance equivalence checks on the exact stress identities that previously diverged. The lifecycle sentinel retains `recycle` as a semantic/performance control.

Paper-facing metrics use the following terminology:

- **post-consensus validation latency**: measured ACG work that gates canonical validation after the
  consensus decision;
- **phase-bottleneck time**: `max(pre_consensus, post_consensus)` and its serial-reference speedup;
  this is a phase-capacity metric, not a claim that block `n+1` overlaps validation of block `n`;
- **sequential end-to-end speedup**: serial reference time divided by the measured non-overlapped ACG
  block time;
- **perfect-conflict lower bound**: hindsight critical path from the serial trace's concrete access
  conflicts, combined with the six-worker capacity bound. It is an evaluation oracle/lower bound,
  not an online scheduler;
- **oracle realization**: actual execution wall time divided by that hindsight lower bound.

All correctness claims are against a deterministic serial execution of the **decided** block.
Candidate pre-execution never commits canonical state before reconciliation.

## ConflictLab 1.0 workload surface

The benchmark contract now exposes multiple state shapes instead of measuring only a single
`Credit(account)` point-key pattern:

| `operation_mix` | Purpose |
|---|---|
| `credit` | Original point-key transfer/credit calibration workload. |
| `point-mixed` | Point reads/writes, counters, approvals, conditional updates, receive-transfer. |
| `stateful-mixed` | Multi-entrypoint state machine including orders, allowance state, configuration, counters, and transfers. |
| `range-delete` | Range scan plus delete/reset interacting with ordinary point writes. |
| `bank-funds` | Contract execution with native-bank balance writes caused by attached funds. |
| `bank-mixed` | Native-bank writes mixed with point/all-balances host queries, exercising runtime-only bank dependencies. |
| `instantiate` | Repeated contract creation and contract-metadata state. |
| `full` | Broad stateful mix combining the main contract-level operations. |

`low`, `medium`, `high`, and deterministic `mixed` Wasm complexity remain execution-cost axes.
The suite records observed serial microseconds/transaction instead of claiming that a synthetic tier
corresponds to one universal blockchain transaction latency.

### Symbolic-analysis ablations

`symbolic_granularity` controls how much analyzer precision is retained:

- `fine`: current field/logical-key analysis;
- `resource`: collapse fields/logical keys to conservative whole resource families; the transformed symbolic artifact uses whole-resource predicates rather than unresolved-key predicates, so this ablation removes key precision without introducing artificial false negatives;
- `profile`: conservatively collapse state-touching profiles into one contract-state envelope, removing field/key distinctions.

`prediction_fault_mode` provides controlled analyzer errors without weakening reconciliation:

- `none`: normal prediction;
- `hidden-key`: a selected transaction exposes a non-conflicting decoy key while Wasm uses the true
  key, creating deliberate false negatives/candidate misses;
- `spurious-key`: selected transactions expose the same decoy key while Wasm uses distinct true keys,
  creating deliberate false positives.

The fault rate is set with `prediction_fault_rate_bps`. These are evaluation injections, not proposed
production analyzer behavior.

### Dense reference ablation

`acg.compact_equivalence_groups=false` disables compact-equivalence materialization while preserving
logical relationships. The resulting path is intentionally expensive and exists only to compare the
compact representation against a dense reference on block sizes where that is practical. It must
not be interpreted as a second production scheduler.


### VM lifecycle correctness gate

The V1 development sweep exposed a specific retained-instance failure mode. `cosmwasm-vm` initializes an instance gas budget when the mutable `Instance` is created; rebinding the host storage/query backend does not create a new meter. ConflictLab's heavy deterministic compute therefore accumulated against one thread-local retained meter across many transactions. The three failure families crossed the same rough cumulative-work boundary: heavy-warmup adaptation (~3.22 billion loop iterations), B2048 block scaling (~3.14 billion expected loop iterations across four warmups plus the measured block), and the long soak (far beyond that boundary), while B1024 remained below it.

V1 retained-mode records therefore set `vm_gas_limit=u64::MAX`. This is intentionally a **non-binding benchmark gas budget**, not a claim that normal chains should use an unbounded per-transaction gas limit. The ConflictLab experiments do not study out-of-gas behavior; gas is only metering overhead here. Retained reuse remains benchmark-scoped because VM-local memory/globals are still not reset in place. Before a retained-mode dataset is accepted, the exact previously failing stress identities must also be rerun under both retained/non-binding-gas and fresh/recycle semantics and produce identical canonical state.

The standard V1 performance grids use retained reuse so VM acquisition does not dominate comparisons with execution engines that amortize runtime construction. `v1-vm-lifecycle` explicitly keeps both `reuse` and `recycle` as the control measuring the size of that lifecycle effect.

## Campaigns and reviewer questions

The corrected full profile contains 4,630 measured records across 15 campaigns. Several campaigns execute
additional warm-up/history blocks; the long-run soak alone executes 1,000 prior blocks per measured
record.

| campaign | measured records | reviewer question answered |
|---|---:|---|
| `v1-core-state` | 960 | Does the complete system help across block size, cost, contention, prediction quality, policy, and admission? |
| `v1-cutoff-divergence` | 1,440 | What survives when consensus arrives early **and** the decided block differs from the candidate? |
| `v1-serial-cutoff` | 72 | Does buffered serial pre-execution correctly reuse a partial prefix under binding cutoffs? |
| `v1-compaction-reference` | 240 | Does compact materialization preserve dense logical semantics, and how much work does it eliminate? |
| `v1-symbolic-granularity` | 72 | What benefit comes specifically from field/logical-key precision rather than resource/profile conflicts? |
| `v1-prediction-fault-recovery` | 336 | Is correctness preserved with missing/spurious symbolic information, and does runtime feedback recover? |
| `v1-adaptation-transitions` | 288 | Does online feedback/admission converge after non-stationary workload changes rather than only after warm-up? |
| `v1-execution-semantics` | 126 | Are point, range/delete, bank, query, state-machine, and instantiate paths actually exercised and serial-equivalent? |
| `v1-block-scaling` | 168 | How do logical relationships, physical materialization, planning, feedback, and execution scale with block size on fixed hardware? |
| `v1-policy-pareto` | 96 | What is the validation/replay versus phase-bottleneck Pareto frontier as scheduling risk changes? |
| `v1-bucket-sensitivity` | 48 | Are conclusions robust to the controlled key-bucketing precision parameter? |
| `v1-ordering-sensitivity` | 36 | Are results an artifact of FIFO block order? |
| `v1-vm-lifecycle` | 24 | How large is the retained-vs-fresh VM lifecycle effect across the four execution-cost tiers, and do paired modes preserve canonical state? |
| `v1-statistical-headlines` | 720 | Are headline results stable across enough independent seeds for distributions/confidence intervals? |
| `v1-long-run-soak` | 4 | Does adaptive state remain correct after a long execution history? |

The exact grids are versioned beside this document as `v1-*.grid.json`. The Python evaluation-tool
unit test asserts their expected sizes, six-worker limit, Wasm backend, and required axis coverage so
accidental matrix drift is detected in the normal repository test gate.

## Main matrix choices

The suite deliberately separates questions rather than multiplying every axis together.

### Core state

Uses B128/B512; low/medium/high/mixed complexity; 25%/75% hotspot contention; exact/bucketed
prediction; static/probability-only/cost-aware policies; admission off/on; five seeds; 500 ms
consensus window. This is broad feature coverage, not the deadline stress campaign.

### Binding cutoff and candidate divergence

Uses B512 high/mixed; exact/bucketed; probability-only/cost-aware; 25%/75% contention;
25/50/100/250/500 ms cutoffs; and identical, reorder-5%, reorder-20%, tail-replace-5%,
tail-replace-20%, and tail+reorder-10% decisions. Three seeds are used. Reporting decomposes decided
transactions into shared/same-position, discarded predictions, missing predictions, invalidated
receipts, replayed transactions, and results ready before cutoff.

### Serial-prefix cutoff

Forces the existing admission path to choose buffered serial pre-execution and sweeps
10/25/50/100/250/500 ms across all four complexity tiers. The validator requires at least one
partially prepared block and one fully prepared block.

### Compaction reference and block scaling

The dense-reference campaign toggles compact groups at B32/B64/B128/B256/B512 under exact/bucketed
prediction. Both halves deliberately train their four warm-up blocks with dense materialization and
a single-worker speculative executor, then apply the compact/dense toggle only to the measured
6-worker block. The normalized warm-up reduces adaptive-history variance, and a deliberately
non-binding 5 s consensus window ensures every measured candidate finishes preexecution.

The two measured halves are still independent six-worker executions. Concrete replay/access evidence
can therefore differ slightly across legal speculative interleavings even when the representation is
unchanged; repeated V1 diagnostics showed that raw positive/negative observation totals are not
bitwise-repeatable across identical run identities. The submission validator consequently treats
post-measured-block feedback totals and posterior means as reported path-variance diagnostics rather
than semantic invariants. It continues to require identical logical candidate counts/classes,
serial-equivalent identical canonical state, equal candidate-miss safety outcomes, complete
preexecution, and non-increasing READY-DAG/materialized representation size. Static dense/compact
pairs retain exact schedule equality. Deterministic Rust regressions separately require the compact
and dense schedulers to produce the same schedule from one feedback checkpoint and require compact
and dense feedback collectors to produce the same update from one identical execution report. This
separates representation correctness from nondeterminism in independently executed parallel paths.

The fixed-hardware block-scaling campaign extends the compact production path through B1024/B2048
under the normal evaluation timing regime. Core count stays six throughout.

### Symbolic precision and faults

Granularity uses `fine/resource/profile` over point-mixed and full stateful operations. Runtime
positive/negative conflict observations plus candidate misses are reported as empirical predictor
precision/recall. Coarse `resource/profile` point-mixed misses are measured rather than treated as
input-resolved validator failures because the ablation intentionally removes key precision and
`ConditionalCredit` retains a state-dependent access guard; `fine` point-mixed remains strictly
miss-free. Fault recovery injects hidden/spurious key errors at 1%, 5%, and 10% and measures the first block after the change
and states after 1/4/8 post-change warm-up blocks. Hidden-key faults must produce candidate misses or
fallback evidence while every decided result remains serial-equivalent.

### Non-stationarity

Adaptation experiments establish an old regime for eight blocks, switch the workload, and sample
post-change depths 0/1/2/4/8/16. Transitions cover low↔high contention and cheap↔expensive execution,
with admission both disabled and enabled. `adaptive_state` telemetry records static/fallback/miss
history and mean probability/confidence at each sampled state.

### Runtime semantics

The semantics campaign uses B256, mixed complexity, exact prediction, both contention levels, and all
three policy modes over point-mixed, stateful-mixed, range-delete, bank-funds, bank-mixed,
instantiate, and full operation mixes. The validator requires the corresponding host/MVCC counters
to be non-zero rather than trusting a manifest label.

### Statistics and long history

Headline configurations use 20 independent seeds for B512 medium/high/mixed, exact/bucketed,
25%/75% contention, all three policies, admission enabled. The soak uses 1,000 preceding blocks per
measured point to detect correctness or adaptive-state failures that only appear after a long history.
For final paper measurements, repeated process-level runs on a clean native-Linux commit should still
be used before reporting high-percentile latency.

## Required invariants

`scripts/validate-conflictlab-v1.py` rejects a complete dataset unless:

- all expected campaign counts are present;
- every record is schema 3, real Wasm, within the fixed six-worker/six-physical-core budget, and
  serial-equivalent;
- the hindsight perfect-conflict lower bound is present and does not exceed serial work;
- compact/dense matched cases preserve logical candidate/edge-class counts and canonical digest;
- short cutoff cases include a genuinely binding deadline and long cases include complete
  pre-execution;
- forced serial cutoff includes partial-prefix and complete-preexecution cases;
- candidate misses remain fatal for input-resolved workloads, while deliberate hidden-key faults,
  runtime-only `bank-mixed` dependencies, and state-derived-key `stateful-mixed`/`full` relationships
  are measured outcomes that must retain fallback or candidate-miss-history evidence; durable
  candidate-miss history is a valid recovery path even when no persistent runtime-fallback
  relationship is installed; spurious-key faults must not create false-negative misses;
- the execution-semantics campaign actually records range scans/removes, bank writes, point and
  all-balances MVCC reads, host queries, contract creation, and stateful deletes;
- adaptation depths and long-run history are present.

A final paper run should also have `git_dirty=false`; the validator reports dirty records so a local
research run can be inspected without silently presenting it as an artifact-ready dataset.

## Running the suite

First run the authoritative repository gate:

```bash
./scripts/run-all-tests.sh
```

Then execute the complete suite:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

The second optional argument chooses a shorter evidence profile:

```bash
./scripts/run-conflictlab-v1-evaluation.sh <output-dir> core
./scripts/run-conflictlab-v1-evaluation.sh <output-dir> mechanisms
./scripts/run-conflictlab-v1-evaluation.sh <output-dir> full
```

`full` is the canonical 4,630-record suite. The runner is resumable: when the same output directory
is reused, a campaign is skipped only if its cached manifest exactly matches the current grid
expansion, its acceptance report is fully accepted, and its record identities/count match that
manifest. Incomplete or stale campaigns are deleted and rerun. Independent campaign failures are
recorded while later campaigns continue, and the suite exits non-zero after post-processing if any
campaign remains invalid. The output directory contains the merged `records.jsonl`,
acceptance/validation output, environment and campaign-count provenance, aggregate CSVs, and a
reviewer-oriented `results-summary.txt`.

Stateful/full execution-semantics runs also distinguish successful and failed speculative receipts.
Failed top-level receipts are legitimate speculative outcomes and may still reconcile correctly;
they are excluded from the successful-only feedback report, so per-transaction service-DAG metrics
are treated as unavailable rather than fabricated when any such receipt is present. Candidate misses
are also classified during V1.0 post-processing as injected prediction faults, runtime-only
dependencies, state-derived symbolic keys, or unexpected input-resolved misses. Schema-3 records only
retain block-level miss counts and adaptive relationship totals, so exact miss-producing profile-pair
attribution is not claimed retroactively.

## Reading the results

Do not collapse the suite to one headline speedup. The analysis should answer, in order:

1. correctness and access-semantics coverage;
2. dense/compact semantic equivalence and scaling;
3. analyzer precision/fault robustness;
4. cutoff completion and candidate/decision survival;
5. post-consensus validation latency;
6. phase-bottleneck time and sequential end-to-end time;
7. policy/admission convergence and tail replay behavior;
8. distance from the hindsight perfect-conflict lower bound;
9. VM/runtime overhead and long-history stability.

External workloads and competitor baselines belong in the next evaluation layer. ConflictLab 1.0 is
intended to make the internal mechanisms independently defensible before those comparisons are made.

### Debug bundle

When V1 post-processing fails, `run-conflictlab-v1-evaluation.sh` writes `validation.txt` and attempts
to create a self-contained upload bundle under `debug-bundles/`. It can also be generated manually:

```bash
./scripts/collect-conflictlab-v1-debug-bundle.sh benchmark-results/conflictlab-v1-core
```

The ZIP contains the combined `records.jsonl`, validation/summary output, campaign manifests and
acceptance reports, compact aggregate CSVs, small correctness-diagnostic artifacts, git provenance,
and snapshots of the V1 validator/policy plus the ConflictLab runtime files most often needed for
triage. Uploading that one ZIP is preferred to pasting the terminal transcript because it lets the
validator failure be reproduced offline without rerunning the benchmark.
