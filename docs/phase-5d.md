# Phase 5D — cost-aware validation/replay feedback

Phase 5D closes the adaptive loop around the Phase 5C.7 execution substrate. It does **not** move
correctness into learned statistics. Predictive graph state continues to control only speculative
launch policy; concrete receipt dependencies, canonical-order validation, atomic reuse, and replay
remain the correctness authority.

## 5D.1 — reconciliation attribution

Post-consensus reconciliation already identified the canonical predecessor whose prepared write set
touched each failed validation dependency. Phase 5D turns that evidence into an explicit
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

Phase 5D introduced checkpoint v2 for replay-cost state. Phase 5E added v3 serialization-cost
state. The current feedback checkpoint is **v4**, adding candidate-miss verification metadata while
remaining backward-readable: v1 restores conflict state, v2 adds replay cost, v3 adds serialization
cost, and v4 adds targeted miss-history recovery state.

## 5D.3 — cost-adjusted scheduling risk

A candidate edge now exposes two separate concepts:

1. `probability()` — the raw learned conflict probability from the existing Beta model;
2. `scheduling_risk()` — the performance-policy risk used by the scheduler.

With no replay-cost evidence, `scheduling_risk == probability`, preserving Phase 4 behavior.

With cost evidence, the policy now targets **combined pipeline execution work** rather than
normalizing replay penalty against one fixed reference. It compares the expected post-consensus
replay work of relaxing the relationship with the pre-consensus serialization work of enforcing it:

```text
fanout_multiplier = 1 + fanout_weight * expected_invalidated_descendants

expected_replay_work =
    conflict_probability
    * expected_replay_cost
    * fanout_multiplier
    * post_consensus_replay_weight

expected_serialization_work =
    effective_serialization_cost
    * pre_consensus_serialization_weight

if replay_work <= serialization_work:
    cost_risk = conflict_probability * replay_work / serialization_work
else:
    cost_risk = 1 - (1 - conflict_probability) * serialization_work / replay_work

scheduling_risk = lerp(
    conflict_probability,
    cost_risk,
    replay_cost_confidence
)
```

At break-even cost the scheduling risk stays equal to the calibrated conflict probability. Cheaper
replay lowers scheduling risk; more expensive replay raises it. The learned serialization cost from
Phase 5E is used when confident, otherwise the configured serialization reference remains the
cold-start fallback.

`RiskBoundedScheduler` uses `scheduling_risk` for hard/soft classification and same-wave soft-risk
accumulation. Raw probability remains available for diagnostics and learning inspection.

Phase 5E now learns marginal dependency-ready delay per relationship and uses `serialization_cost_reference_nanos` only as the low-confidence fallback. See `phase-5e.md`.

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

The Phase-5D regression tests are part of the canonical repository gate:

```bash
./scripts/run-all-tests.sh
```

## Consensus/correctness invariant

Replay wall time is machine-local and nondeterministic. It is therefore **not consensus data**.
Different validators may learn different cost estimates and choose different speculative schedules.
That is allowed because the speculative schedule is an optimization hint only; canonical execution
semantics and block validity must not depend on the learned cost state.

## Current limitations / follow-on work

Phase 5D intentionally stops short of several useful refinements:

- replay cost is learned at profile-relationship level, not individual symbolic clause level;
- fan-out is represented as a count and converted to cost by a configurable multiplier rather than
  summing a learned descendant-cost distribution;
- there is no explicit exploration policy for periodically probing long-lived soft/hard edges;
- wall-time evidence is local and should never be used in consensus-visible output.

Those are candidates for later adaptive-policy work, not requirements for the Phase 5D correctness boundary.
