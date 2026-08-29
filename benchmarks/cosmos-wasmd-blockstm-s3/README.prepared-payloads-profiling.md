# Wasmd prepared payloads + isolated comparison profiling

This follow-on optimization removes measured transaction-path work that is
invariant after benchmark setup:

- logical address -> Bech32 replacements are materialized once per `benchApp`;
- workload execute/query JSON is rewritten and marshaled once per app;
- workload funds/bank coin vectors and resolved account/contract addresses are
  prepared once per app;
- replays reuse these immutable prepared payloads.

The benchmark still performs the same Wasmd/WasmVM/keeper calls and preserves
source-failed and source-revert cache semantics. The fingerprint access tracker
remains unchanged.

The 2-worker diagnostic script also enables isolated profiles for:

1. `direct-serial`
2. `outer-cache-serial`
3. `symbgraph-static`

CPU/allocation and mutex profiles execute in separate fresh processes so mutex
sampling cannot distort CPU results. Allocation profiles are written before and
after the replay; use `go tool pprof -diff_base=before after` to subtract app
setup. `scripts/show-vegeta-s3-wasmd-compare-profiles.sh` prints the standard
comparison, including `outer-cache - direct` CPU differential output.
