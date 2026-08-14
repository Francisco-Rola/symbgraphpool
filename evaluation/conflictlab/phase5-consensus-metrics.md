# Phase 5 consensus-window-aware metrics

Phase 5 treats the consensus execution window explicitly. The key architectural fact is that both speculative ACG execution and direct-serial fallback are eligible to start before consensus decides the block. A serial fallback is therefore **not** charged as if its entire execution starts after consensus.

The original non-overlapped end-to-end speedup remains available as a secondary total-work diagnostic.

## Pre-consensus window

Let `W` be the wall-clock budget available between the candidate block becoming available for pre-execution and consensus deciding that block.

For every ACG path, including serial fallback, define:

- `eligible_pre = planning + preexecution + pre_execution_feedback`
- `intrinsic_post = reconciliation + reconciliation_feedback`

With a finite window:

- `pre_completed = min(eligible_pre, W)`
- `pre_spill = max(eligible_pre - W, 0)`
- `post_validation = pre_spill + intrinsic_post`

If no window is supplied to the reporting tools, they report the upper-bound/full-preexecution case: all eligible work is assumed to complete before consensus, so `pre_spill = 0`.

The current ConflictLab benchmark prepares and decides the same deterministic block. The metric therefore assumes any pre-executed result is still valid at consensus. Proposal/decision divergence and partial result invalidation are a separate evaluation dimension and must not be inferred from these records.

## Serial fallback

A Phase 5 serial fallback performs direct canonical execution, but it can perform that execution speculatively before consensus just like any other pre-execution strategy.

If the full serial fallback completes inside `W`, the measured execution remainder after consensus is zero. If it only completes a prefix before consensus, the unfinished measured serial work spills into `post_validation`. The already completed canonical prefix is treated as reusable under the same-block validity assumption above.

This is a reporting model over the currently recorded aggregate wall timers. A future consensus-window benchmark should interrupt execution at the actual decision boundary and record the exact completed/reused transaction count rather than infer the remainder from aggregate wall time.

## Block validation latency

The conventional serial validator is assumed to begin canonical execution only after consensus:

`serial_validation_latency = serial_reference_execution_nanos`

ACG validation latency is:

`acg_validation_latency = pre_spill + intrinsic_post`

and:

`validation_latency_speedup = serial_validation_latency / acg_validation_latency`

If ACG has completed all eligible work and has no intrinsic post-consensus work (possible for the current direct-serial fallback benchmark), measured post-consensus execution latency is zero and the ratio is reported as unbounded/undefined rather than converted into a finite claim.

## Execution-limited throughput

At a finite consensus window the two execution stages are:

- pre-consensus stage = `pre_completed`
- post-consensus stage = `post_validation`

The pipeline service time is:

`acg_block_service_time = max(pre_completed, post_validation)`

The conventional serial design has no pre-consensus execution and therefore uses:

`serial_block_service_time = serial_reference_execution_nanos`

For a block containing `N` transactions:

`acg_throughput_tps = N / acg_block_service_time`

`serial_throughput_tps = N / serial_block_service_time`

`throughput_speedup = serial_block_service_time / acg_block_service_time`

These are execution-limited throughput metrics and assume work assigned to adjacent pre/post pipeline stages can overlap as intended by the architecture. Real chain throughput can still be limited by consensus cadence or shared execution resources. A continuous multi-block pipeline benchmark is required to validate this analytical service-rate model directly.

## Secondary total-work metric

The historical Phase 1-5 metric remains:

`total_work_speedup = serial_reference_execution_nanos / total_adaptive_block_nanos`

It measures serial wall time against the non-overlapped sum of ACG planning, preexecution, feedback, reconciliation, replay, and post-feedback. It remains useful for compute-efficiency/accounting, but it is not the primary validation-latency or pipelined-throughput metric for this architecture.
