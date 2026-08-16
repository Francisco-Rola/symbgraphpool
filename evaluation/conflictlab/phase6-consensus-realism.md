# ConflictLab Phase 6: consensus-cutoff realism

Phase 6 evaluates the ACG execution design at the candidate-block / decided-block boundary. It does
not overlap pre-execution of block `n+1` with validation of block `n`: the throughput metric is a
service-time comparison only, defined by the slower of the measured pre-consensus and post-consensus
phases for one block.

## Physical consensus cutoff

`consensus_cutoff_ms` is a real pre-execution launch deadline. Planning consumes the same budget.
Once the deadline is reached, the dependency executor stops launching new transactions. Contract
calls already in flight are not cancelled; they are allowed to finish and their overrun is charged
to post-consensus latency. Phase 6 evaluates 250 ms, 500 ms, and 1000 ms cutoffs.

The record distinguishes:

- receipts completed by the cutoff;
- receipts launched before the cutoff but completed after it;
- whether the cutoff prevented additional launches;
- measured pre-consensus work, cutoff overrun, post-consensus work and `max(pre, post)` service time.

## Buffered serial pre-execution

Admission may decide that canonical serial pre-execution is the best predicted strategy. This is
still pre-execution: it runs with one worker against a detached snapshot/MVCC receipt buffer. No
canonical state is committed before consensus. The same physical cutoff applies, so only the prefix
that can be launched before the decision is prepared. Reconciliation validates/reuses eligible
receipts against the decided block and executes missing or stale transactions canonically after
consensus.

A bounded serial-bypass streak (default four blocks) forces one normal adaptive admission probe so a
changed workload cannot become permanently stuck in the serial state. This does not alter scheduler
risk or explore extra candidate relationships.

## Candidate / decided block divergence

The candidate block is generated before consensus. A second deterministic block is generated from
the same initial state and transformed into the decided block. The serial reference always executes
the decided block. ACG pre-executes the candidate and reconciles against the decision.

Supported divergence modes are:

- `identical`;
- `tail-5pct` and `tail-20pct`: replace the corresponding tail with alternate transactions;
- `reorder-5pct` and `reorder-20pct`: reverse that tail while preserving transaction identity;
- `tail-reorder-10pct`: replace and reorder a 10% tail.

Feedback attribution is aligned by transaction ID so reordering or replacement cannot train a
candidate edge against an unrelated decided-block index.

## Primary metrics

For the conventional serial baseline, block validation latency is the measured canonical execution
of the decided block after consensus.

For ACG, block validation latency is the work remaining on the critical path at the decision:
pre-consensus overrun plus reconciliation/receipt validation, required replay or missing execution,
commit and synchronous reconciliation feedback.

Execution-limited service time is:

```
max(measured_pre_consensus_time, measured_post_consensus_time)
```

and throughput speedup is serial decided-block execution time divided by that service time. This is
not a claim that different blocks overlap in the implementation; it is the requested throughput
comparison of the two phases of this block. Consensus/network throughput can impose an independent
cap.

The older non-overlapped `serial / total_adaptive_block` speedup remains a secondary total-work
metric.

## Transaction-cost bands

ConflictLab remains synthetic, so these are workload bands rather than claims about a universal
blockchain transaction latency. The evaluation reports the *observed* serial microseconds per
transaction in every run so the bands self-calibrate on the benchmark host.

- low: 8,192 compute iterations, 1 storage round, 96-byte payload; intended to cover inexpensive
  native/transfer-like or very small contract work;
- medium: 131,072 iterations, 3 storage rounds, 512-byte payload; intended to cover ordinary
  stateful application transactions;
- high: 786,432 iterations, 6 storage rounds, 2,048-byte payload; intended to cover expensive
  contract/state work and make the 250 ms cutoff observable at B512;
- mixed: deterministic 33/34/33 low/medium/high composition unless a matrix states another mix.

The purpose is broad service-cost coverage. Real chain execution times vary substantially by VM,
host functions, state backend, hardware and transaction type, so observed serial service time is the
quantity to use when interpreting results.

## Current status

This document describes the pre-V1 Phase 6 design. Its cutoff, divergence, policy, and VM-lifecycle
coverage is now subsumed by ConflictLab 1.0. Use the current suite instead of the historical Phase 6
runner:

```bash
./scripts/run-conflictlab-v1-evaluation.sh
```

## Repository gate

Run after every patch:

```bash
./scripts/run-all-tests.sh
```

It runs `cargo fmt --all` for every Cargo workspace, `git diff --check`, shell/Python syntax checks,
all Rust workspace/all-target tests, Rust doc tests, Clippy with `-D warnings`, and the evaluation-tool
test suite. The expensive 952-run benchmark campaign is intentionally separate.
