# Brick 5D — cost-aware validation/replay feedback

Brick 5D closes the adaptive loop around the Brick 5C.7 execution substrate. It does **not** move
correctness into learned statistics. Predictive graph state continues to control only speculative
launch policy; concrete receipt dependencies, canonical-order validation, atomic reuse, and replay
remain the correctness authority.

## 5D.1 — reconciliation attribution

Post-consensus reconciliation already identified the canonical predecessor whose prepared write set
touched each failed validation dependency. Brick 5D turns that evidence into an explicit
`ReplayAttribution` containing:

- predecessor and replayed `TxIndex`;
- the exact `ValidationConflict` (storage key/range, balance, all-balances, or contract metadata);
- derived conflict kinds;
- measured canonical re-execution wall time attributed to that cause;
- transitive replay fan-out through the concrete reconciliation evidence graph;
- whether the responsible transaction relationship existed in the candidate graph.

`CanonicalTxResult` and validator reconciliation diagnostics now retain the canonical execution
duration paid by replay/missing transactions. Reused receipts report zero re-execution duration.
When one replay has multiple validation conflicts, its direct duration is divided across those
attributions while preserving the exact measured total.

Fan-out is derived from **concrete reconciliation dependency evidence**, not from the predicted DAG.
It therefore measures the downstream replay cascade actually observed in that reconciliation.

## 5D.2 — persistent replay-cost state

`AdaptiveFeedbackStore` now maintains a second decayed statistic beside the existing Beta conflict
posterior for every static and runtime-discovered profile relationship:

- weighted replay nanoseconds;
- weighted invalidated-descendant count;
- decayed observation weight;
- replay observation count;
- cumulative raw replay nanoseconds and fan-out.

The projected mean replay cost/fan-out stays stable under pure decay while confidence falls as the
observation weight decays. New independence/conflict evidence continues to update the existing Beta
posterior independently.

Feedback checkpoint format version is now **2**. Version-1 checkpoints remain readable; their new
replay-cost state starts empty at the old edge statistic's last-update epoch.

## 5D.3 — cost-adjusted scheduling risk

A candidate edge now exposes two separate concepts:

1. `probability()` — the raw learned conflict probability from the existing Beta model;
2. `scheduling_risk()` — the performance-policy risk used by the scheduler.

With no replay-cost evidence, `scheduling_risk == probability`, preserving Brick 4 behavior.

With cost evidence, the current policy computes:

```text
fanout_multiplier = 1 + fanout_weight * expected_invalidated_descendants

expected_speculation_penalty =
    conflict_probability
    * expected_replay_cost
    * fanout_multiplier

cost_risk = clamp(
    expected_speculation_penalty / serialization_cost_reference,
    0,
    1
)

scheduling_risk = lerp(
    conflict_probability,
    cost_risk,
    replay_cost_confidence
)
```

`RiskBoundedScheduler` uses `scheduling_risk` for hard/soft classification and same-wave soft-risk
accumulation. Raw probability remains available for diagnostics and learning inspection.

The `serialization_cost_reference_nanos` is deliberately an explicit policy input rather than a
claim that the system already has a perfect online lost-parallelism estimator. Productizing that
reference is part of Brick 5E.

## 5D.4 — closed-loop phase adaptation

The new closed-loop evaluation drives one known symbolic relationship through three phases:

1. repeated concrete independence matures/softens it;
2. one expensive, high-fan-out replay hardens it through cost-adjusted risk;
3. a later independent phase lowers the conflict posterior while old cost confidence decays, so the
   relationship softens again.

The test also checkpoints/restores the expensive phase and requires the scheduling decision to be
preserved.

A separate split-phase integration test deliberately over-speculates conflicting transactions and
verifies that replay duration, exact concrete cause, candidate-edge presence, and transitive fan-out
are attributed and reach the persistent feedback store.

Run the focused evaluation with:

```bash
./scripts/run-brick5d-closed-loop-diagnostics.sh
```

## Consensus/correctness invariant

Replay wall time is machine-local and nondeterministic. It is therefore **not consensus data**.
Different validators may learn different cost estimates and choose different speculative schedules.
That is allowed because the speculative schedule is an optimization hint only; canonical execution
semantics and block validity must not depend on the learned cost state.

## Current limitations / follow-on work

Brick 5D intentionally stops short of several useful refinements:

- replay cost is learned at profile-relationship level, not individual symbolic clause level;
- fan-out is represented as a count and converted to cost by a configurable multiplier rather than
  summing a learned descendant-cost distribution;
- the serialization-cost reference is configured, not learned online from realized lost
  parallelism;
- there is no explicit exploration policy for periodically probing long-lived soft/hard edges;
- wall-time evidence is local and should never be used in consensus-visible output.

Those are candidates for Brick 5E metrics/policy productization and later adaptive-policy work, not
requirements for the Brick 5D correctness boundary.
