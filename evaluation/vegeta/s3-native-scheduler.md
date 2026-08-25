# Vegeta S3 native scheduler evaluation

This experiment is the performance companion to the exact SLOAD/SSTORE topology-fidelity study.
It executes the translated 101-block / 13,783-transaction Vegeta S3 workload through the real
CosmWasm engine and compares seven scheduler mechanisms against a matched serial engine.

The publication configuration is frozen in `s3-native-scheduler.v1.json`. No speedup, replay-rate,
post-consensus-latency, or other performance threshold is used for acceptance. Mechanical
validation checks completeness, provenance labels, serial-equivalent state, and the previously
completed topology/family-freeze prerequisites only.

## Strategy semantics

- `serial`: canonical post-consensus serial execution.
- `aria-fb`: same-VM post-consensus fully-parallel speculative discovery followed by an
  Aria Rule-2-like fallback filter and canonical reconciliation. This is a mechanism adaptation,
  not a claim of source-code identity with Aria.
- `vegeta`: same-VM pre-consensus concrete discovery adaptation. A conflict DAG is discovered from
  detached execution and replayed/validated after the consensus cutoff. This is a mechanism
  adaptation, not the paper's original EVM implementation.
- `exact-access`: evaluation-only information oracle. Current-block concrete conflict edges come
  from a matched serial control and are not deployable pre-consensus information.
- `static`: checked-in source-derived symbolic profiles and public translated call inputs only.
  It never reads historical source EVM storage keys or current-block concrete native accesses.
- `probability-only`: the static symbolic prior plus conflict probabilities learned from strictly
  prior canonical blocks.
- `cost-aware`: the same prior plus strictly-prior-block execution cost, with a frozen projected
  speedup threshold for serial bypass.

Each strategy/sample gets independent matched-serial and strategy engines initialized from the same
manifest, bank seeds, native instances, and priming calls. Both engines advance through all 101
blocks in canonical order. After every block the full contract+bank world state must match.

Performance includes native bank-ledger dependencies because they are real dependencies of the
CosmWasm workload. This differs deliberately from the storage-to-storage source-fidelity metric,
which excludes bank ledger accesses because the Ethereum source corpus contains storage slots, not
account-balance ledger keys.

## Consensus-relative timing

Planning and eligible detached pre-execution share the frozen consensus-cutoff budget. Work that
finishes after the cutoff is charged to post-consensus latency. Reconciliation/replay and adaptive
feedback processing are also charged post-consensus. The records report both active wall speedup
and post-consensus speedup versus the matched serial block. `consensus_bottleneck_nanos` follows the
existing harness convention: `max(pre_consensus_nanos, post_consensus_nanos)`.

## Candidate-archetype freeze

`cw1155-like` and `operator-filter-helper` were historically labelled candidate archetypes. The
freeze evaluator first removes their selector rules and recomputes the already-frozen semantic
coverage gates. If those gates no longer pass, freeze readiness is still allowed only when both
families are proven to have checked-in real CosmWasm implementations and genuine source-derived
symbolic profiles with `historical_trace_keys_used=false`. No threshold is relaxed.
