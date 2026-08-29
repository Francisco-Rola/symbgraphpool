# Wasmd compact exact-access tracker

This patch replaces the hot exact-access representation used by the Wasmd
SymbGraph and Vegeta ports.

* `Get/Has/Set/Delete` no longer convert arbitrary KV keys to Go strings.
* Store capabilities are memoized as deterministic numeric IDs.
* Exact accesses use allocation-free 64-bit fingerprints and pre-sized maps.
* Unique writes retain one raw key copy because iterator/range validation must
  compare ordered bytes exactly. Hash collisions are conservative: they can
  create false conflicts/replay, but cannot hide a conflict on the same key.
* Iterator boundaries remain exact byte slices.
* The 2-worker diagnostic script enables a separate SymbGraph CPU profile and
  records `runtime.MemStats` allocation deltas around the 101-block profile run.

Use `scripts/run-vegeta-s3-wasmd-optimized-w2.sh` first. Inspect:

```
.../symbgraph-pprof/cosmos-wasmd-symbgraph-static-w2.cpu.pprof
.../symbgraph-pprof/cosmos-wasmd-symbgraph-static-w2.profile.json
```

Then run `scripts/run-vegeta-s3-wasmd-optimized-sweep.sh` if state equivalence
holds and the tracked-serial control improves.
