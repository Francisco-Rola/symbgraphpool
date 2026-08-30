# Rust ACG Wasmd bottleneck and risk-policy evaluation

This diagnostic layer sits on top of the optimized Rust-ACG Wasmd bridge. It does not change the default scheduler policy unless an explicit `--symbgraph-rust-*` policy override is supplied.

## Dependency reason accounting

When dependency diagnostics are explicitly enabled, each projected transaction dependency carries one or more reasons from Rust. The normal publication path leaves this disabled so JSON reason construction/serialization does not perturb timing:

```bash
--symbgraph-rust-dependency-diagnostics
```

The risk-sweep script enables it automatically.

Reasons:

- `symbolic_hard`: a predicate-proven static relationship that the scheduler treats as hard;
- `adaptive_hard`: a learned/runtime or probability/cost-driven relationship currently classified hard;
- `soft_risk`: a soft relationship that the risk-bounded scheduler chose to serialize;
- `projection_hard`: a hard component relationship restored after atomic parent projection;
- `bank_resource`: an exact adapter-level bank/funds resource chain outside CosmWasm symbolic storage.

`projection_hard` is a mechanism flag and may accompany a semantic `symbolic_hard` or `adaptive_hard` reason. `symb_dependency_primary` selects one semantic primary reason per retained parent dependency so its counts sum to the dependency count. Critical-path cost attribution assigns each successor transaction's estimated cost to the primary reason of the critical-path edge that gates it; the first transaction cost is recorded under `root`.

The Wasmd JSONL rows expose:

- `symb_dependency_reasons`
- `symb_dependency_primary`
- `symb_critical_path`
- `symb_critical_path_reasons`
- `symb_critical_path_cost_by_reason`
- `symb_planning`

This allows the evaluation to distinguish an unavoidable hard critical path from policy-created serialization.

## Risk-policy sweep

Run:

```bash
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-risk-sweep.sh
```

The debug default compares `default,risk40,risk60,soften4,moderate,aggressive`. It uses only matched direct serial plus Rust-ACG for these policy diagnostics so repeated BlockSTM/Vegeta executions do not dominate experiment time.

The named policies are deliberately staged:

- `default`: repository defaults;
- `risk40`: only the aggregate soft risk budget increases to 0.40;
- `risk60`: only the aggregate soft risk budget increases to 0.60;
- `soften4`: only hard-to-soft evidence maturity drops from 8 to 4 independent observations;
- `moderate`: soft=0.30, hard=0.90, risk=0.50, maturity=4, softening confidence=0.15;
- `aggressive`: soft=0.40, hard=0.95, risk=0.70, maturity=2, softening confidence=0.10 plus 10% bounded exploration.

The point is not to minimize replay. The desired operating point maximizes useful parallelism after replay cost.

Useful overrides:

```bash
VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS=2,4,6 \
VEGETA_S3_RUST_ACG_RISK_SWEEP_SAMPLES=3 \
VEGETA_S3_RUST_ACG_RISK_SWEEP_POLICIES=default,risk40,risk60,moderate,aggressive \
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-risk-sweep.sh
```

### WSL / 6-core, 12-thread host

The sweep detects physical cores using `lscpu` when available and defaults to 2/4/6 workers on a 6-core host. Keep 8/12 workers as a separate SMT/oversubscription experiment:

```bash
VEGETA_S3_RUST_ACG_RISK_SWEEP_WORKERS=2,4,6,8,12 \
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-risk-sweep.sh
```

Interpret 8/12-worker results as simultaneous-multithreading behavior, not evidence that the scheduler has eight or twelve physical execution cores available.

Outputs:

```text
.../rust-acg-risk-sweep/summary/risk-sweep.txt
.../rust-acg-risk-sweep/summary/risk-sweep.csv
.../rust-acg-risk-sweep/<policy>/bottlenecks/worst-blocks.txt
```

## Worst-block analysis

Any JSONL produced after this diagnostic patch can be inspected with:

```bash
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-bottlenecks.sh path/to/records.jsonl
```

For each worker count the report sorts the slowest blocks and shows wall time, planning/preexecution/reconciliation/replay/feedback costs, worker utilization, DAG parallelism, the concrete critical-path transaction indices, all primary dependency reason counts, critical-path reason counts, and critical-path cost attribution.

A useful interpretation sequence is:

1. If `soft_risk` or `adaptive_hard` dominates critical-path cost and more aggressive policies reduce it while replay remains affordable, tune the Rust risk policy.
2. If `symbolic_hard` dominates, inspect those predicate-proven relationships and the workload's actual conflicts before weakening anything.
3. If `bank_resource` dominates, the bottleneck is outside contract symbolic storage and requires a Cosmos-bank-specific strategy rather than ACG predicate tuning.
4. If DAG parallelism is high but measured worker utilization remains low at <= physical-core count, return to the Go/Wasmd executor rather than weakening the graph.

## Oracle DAG, planner subphases, and Vegeta-style post-consensus speedup

The focused diagnostics pass adds an after-the-fact **actual-access oracle DAG** built from the final canonical Wasmd read/write fingerprints. It is diagnostic only: it uses information that is not available before execution. Compare `symb_dag_parallelism` with `symb_oracle_dag_parallelism`; `symb_serialization_gap = oracle/acg` quantifies theoretical scheduling headroom. A value near 1 means the predicted ordering graph is already close to the actual-conflict bound; a larger value means the ACG serialized work that the final concrete accesses show could have overlapped.

Rust planning is split into request construction/marshal, Rust JSON decode, component/profile resolution, weighted candidate-graph construction, `RiskBoundedScheduler`, parent projection, profile-feedback-pair construction, finalization, response unmarshal, and an unclassified bridge remainder. These timings are emitted only when dependency diagnostics are enabled so publication timing does not pay the diagnostic accounting cost.

Dependency diagnostics now report two independent dimensions. **Provenance** is stable under policy changes (`static_predicate`, `static_profile`, `runtime_discovered`, `bank_resource`, `projection`). **Decision** reports what the current policy did (`hard`, `soft_serialized`, `bank_hard`, `projection_restore`). Candidate classification counters additionally report hard/soft/low relationships and retained hard/soft ordering dependencies.

All publication and focused summaries now report two speedups:

- `net-x`: matched direct serial / full scheduler wall time.
- `post-x`: matched direct serial / consensus-visible post phase. This is the Vegeta-style metric: pre-consensus planning/speculation is treated as free provided it fits within the available consensus window. For Rust-ACG the post phase is canonical reconciliation/validation/replay; for Wasmd Vegeta it is commit/replay; BlockSTM has no separate pre-consensus stage and therefore keeps its full execution time in the post phase.

Always interpret `post-x` together with the measured pre-consensus time. If pre-consensus work exceeds the consensus window it is no longer operationally free even though the post-only metric remains high.

Run the focused second-stage policy experiment on a 6-physical-core host with:

```bash
bash tools/legacy-scripts/run-vegeta-s3-rust-acg-focused-diagnostics.sh
```

The default is workers `4,6`, three full samples, and these policies: `default`, `thresholds-only`, `aggressive-no-exploration`, `moderate+exploration`, `aggressive`. This isolates threshold movement, aggressive softening/risk without exploration, and bounded exploration instead of repeating the broad first sweep.

## Candidate-graph planning diagnostics

The graph-planning optimization keeps symbolic parsing, profile conflict derivation, predicate
compilation, and profile topology in persistent Rust state. Re-run the focused diagnostics after the
optimization and compare the planning subphases together with the new physical/logical candidate and
parent-reduction counters. A successful compaction pass should reduce `plan_graph_ms`,
`plan_scheduler_ms`, and/or `plan_projection_ms` without changing the logical candidate relationship
count or canonical execution result.

With the atomic candidate path, `phys` counts physical atomic transaction-pair relationships.
When several distinct profile relationships contribute evidence to the same pair, Rust stores them
inside one parallel-evidence group and the scheduler reproduces their original classification and
soft-risk composition without adding duplicate adjacency edges. `log` still counts the concrete
component relationships represented by those physical pairs, so a large `log/phys` ratio is expected
and directly measures online graph compaction. `grp` remains the number of clique/equivalence compact
groups. For worst blocks, `cand[p/l/g]` uses those same values. `parent[b/e]` is the already-atomic
scheduler/bank dependency count before the final adapter-level reduction / dependencies elided by
that reduction; there is no longer a component-to-parent projection stage.
