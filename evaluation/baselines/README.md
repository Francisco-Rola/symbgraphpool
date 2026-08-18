# Cross-strategy baseline framework

This directory is the first publication-oriented comparison layer above ConflictLab. All strategies
run the same generated blocks against the same `CosmWasmEngine`, VM lifecycle, initial state,
worker budget, and serial correctness oracle. This deliberately removes VM/storage differences from
the first comparison.

Supported `RunIdentity.mode` values:

- `serial`: canonical decided-order execution.
- `aria-fb`: AriaFB-style order-execute batch OCC. The block executes in parallel against one
  snapshot, Rule-2 forward dependencies are proactively sent to fallback, and the concrete read-set
  validator may conservatively replay additional transactions so the result remains exactly equal
  to the consensus-decided order. This is a same-VM canonical-order adaptation, not a claim that the
  code is the upstream Aria implementation.
- `vegeta`: speculate-order-replay inspired by Vegeta. Candidate transactions execute fully in
  parallel before consensus, concrete R/W conflicts build a replay DAG, and the decided block is
  replayed with that DAG. A changed R/W footprint forces deterministic fallback. Rule-1 transaction
  reordering is intentionally disabled because ConflictLab treats decided transaction order as the
  canonical state-machine order. The record identifies the implementation as `vegeta-like`.
- `exact-access`: evaluation-only hindsight oracle. Concrete storage/bank accesses from the paired
  serial reference build the detector-complete conflict DAG before candidate pre-execution, while
  the concrete read-set validator remains the final semantic boundary. It currently requires
  identical candidate and decided transactions and must never be presented as deployable.
- `static`, `probability-only`, `cost-aware`: the existing SymbGraphPool modes.

The design follows the published structure of Aria/AriaFB and Vegeta while preserving a stronger
common correctness boundary. The primary sources are Aria (PVLDB 2020) and Vegeta (NSDI 2025).
The next publication phase should additionally run upstream/native implementations where feasible;
these same-VM baselines are for controlled mechanism comparison.

Run the initial 63-record smoke matrix with:

```bash
./scripts/run-baseline-comparison.sh
```

Every record must match the paired canonical serial state digest. Baseline-specific counters are in
`record.strategy`, while existing timing/replay fields remain available to the normal aggregation
tools.

## Publication reporting

The smoke report uses **matched direct-Serial normalization** for cross-strategy comparisons: each
strategy's actual block wall is divided into the direct `serial` strategy's actual block wall for the
same workload, worker budget, seed, and parameters. This keeps the per-record paired serial reference
as an internal timing diagnostic without letting independent serial timing noise reorder strategies.
The report prints min/median/max matched speedup by mode and by contention, and writes the per-run
normalization table to `baseline-matched-serial.csv`.

The current smoke grid deliberately disables serial admission/bypass and regime-change handling so
all strategies are forced through the controlled mechanism comparison. Those controls are printed in
the report and should be treated as an ablation, not the production SymbGraphPool configuration.

For a result intended to be cited in a paper, run from a committed clean tree:

```bash
./scripts/run-baseline-comparison.sh --publication
```

`--publication` refuses to start when `git status --porcelain` is non-empty and records the clean-tree
status in `environment.txt`. The 63-record, three-seed synthetic matrix remains a smoke test even in
publication mode; publication-scale external workloads and seed counts are separate campaigns.
