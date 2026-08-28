# Vegeta S3 replay/runtime and workload-cost diagnostics

This patch adds two diagnostic evaluations without changing the default seven-strategy scheduler run.

## 1. Direct DAG replay scaling

Run:

```bash
bash scripts/run-vegeta-s3-replay-scaling.sh
```

Defaults to worker counts `1,2,4,6,8,16` and three samples. Override with:

```bash
VEGETA_S3_REPLAY_WORKERS=1,2,4,8,16 \
VEGETA_S3_REPLAY_SAMPLES=3 \
bash scripts/run-vegeta-s3-replay-scaling.sh
```

The diagnostic runs only `serial`, `exact-direct`, and `exact-access`:

- `exact-direct`: exact concrete conflict DAG, persistent ready-DAG worker pool, deferred worker execution, batched coordinator commits, no speculative receipts, or receipt validation.
- `exact-access`: the existing exact dependency/MVCC preexecution path.
- `serial`: matched canonical control.

Primary comparison:

- `exact-direct replay_speedup` versus `exact-access preexecution_speedup`.

Both `exact-direct` and the generic dependency/MVCC executor now reuse persistent worker pools across blocks. The S3 execution and scheduler binaries also force `WasmInstanceLifecycle::Reuse`, so the scaling sweep does not repeatedly pay thread creation or mutable-instance recycle costs. Scheduler validation rejects records that do not report `wasm_instance_lifecycle="reuse"`.

If direct replay scales but exact-access preexecution does not, the remaining gap is in the speculative/MVCC substrate rather than worker creation or the workload dependency graph. If both fail to scale, inspect native transaction cost, VM/cache contention, state-read contention, and weighted critical-path concentration.

The same worker pool is reused across all 101 blocks in a strategy sample. Generic dependency/MVCC preexecution also caches a Rayon pool by worker count inside the engine, so worker creation is not charged once per block. All native S3 execution/scheduler entrypoints force `WasmInstanceLifecycle::Reuse`, with `u64::MAX` gas, so the VM lifecycle is held constant across serial and parallel strategies.

Outputs are written under `native-execution/replay-scaling/` by default.

## 2. EVM-to-native transaction cost fidelity

Run:

```bash
bash scripts/run-vegeta-s3-cost-fidelity.sh
```

This replays the native plan serially while recording `native_execution_nanos` per transaction, joins those rows to the frozen exact EVM traces by `(block, tx_index, tx_hash)`, and compares native wall cost with source `gasUsed` and `steps`. The native replay also uses `WasmInstanceLifecycle::Reuse`.

The checked-in exact trace set is allowed to omit up to two source transactions by default (`VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS=2`) because those traces could not be recovered from the public RPC. Missing transactions are listed in `summary.json`. Correlation uses every matched transaction, while any block containing a missing source trace is excluded from weighted critical-path analysis so an incomplete DAG is never treated as exact.

By default the runner permits the two S3 source transactions whose public-RPC SLOAD/SSTORE traces are unavailable (`VEGETA_S3_ALLOWED_MISSING_SOURCE_TRANSACTIONS=2`). All matched transactions still participate in native/EVM cost correlation. Any block containing a missing source trace is explicitly excluded from critical-path reconstruction so an incomplete conflict DAG is never treated as exact. Missing transaction details and excluded block numbers are written to `summary.json`.

It reconstructs the source RAW/WAR/WAW conflict DAG for complete blocks and reports source weighted critical paths. The key diagnostics are:

- Pearson/Spearman correlation of native execution time with EVM `steps` and `gasUsed`.
- Native CPU share on the source steps-weighted critical path.
- Source opcode-step share on that same path.
- `critical_path_native_overweight_ratio = native critical-path cost share / source critical-path step share`.

A ratio materially above `1` means the semantic port concentrates more CPU on the serialized source critical chain than the EVM workload does, which suppresses visible parallel speedup even if access topology is preserved.

Outputs are written under `native-execution/cost-fidelity/` by default.

## Validation

Python diagnostics/tests:

```bash
python3 -m unittest scripts.tests.test_vegeta_native_s3_diagnostics scripts.tests.test_vegeta_native_s3_scheduler -v
```

Rust/runtime tests (requires the repository Rust toolchain):

```bash
cargo test --manifest-path runtime/Cargo.toml -p acg-validator-sim
cargo test --manifest-path runtime/Cargo.toml -p acg-vegeta-native-s3-executor --bin acg-vegeta-native-s3-benchmark
```

The direct replay evaluation additionally fails if any measured strategy diverges from matched serial world state.

## EVM-cost calibrated compute sweep

The semantic CosmWasm port intentionally preserves state/conflict behavior rather than EVM opcode
cost. The cost-fidelity diagnostic can therefore show good topology while transaction service times
are too small or differently distributed to reproduce Vegeta-style replay scaling.

The calibrated sweep adds a **state-free deterministic CPU supplement** to each translated source
transaction. It does not add reads, writes, crypto state, or synthetic conflicts. The supplement is
weighted by the frozen source trace's `steps` or `gasUsed` and executes before the translated bundle,
so even source-reverted transactions retain their source-cost weight.

At scale `1`, the default configuration adds approximately `1000 ms` of aggregate single-core CPU
work across the full 101-block S3 range, distributed by the selected source metric. The loop
throughput is calibrated once at the start of the sweep and the same iterations/ns value is pinned
for every worker-count/profile run. Scales therefore change the deterministic iteration count rather
than re-timing the calibration primitive independently for each run.

The default experiment is:

```bash
bash scripts/run-vegeta-s3-compute-calibration-sweep.sh
```

Defaults:

```text
metrics:    steps, gas
scales:     0.25, 0.5, 1, 2, 4
workers:    1, 2, 4, 8, 16
strategies: serial, exact-direct, exact-access
samples:    1
base CPU:   1000 ms at scale 1
```

For a publication-quality selected profile, increase samples after the exploratory sweep, e.g.:

```bash
VEGETA_S3_COMPUTE_METRICS=steps \
VEGETA_S3_COMPUTE_SCALES=1,2 \
VEGETA_S3_COMPUTE_SAMPLES=3 \
  bash scripts/run-vegeta-s3-compute-calibration-sweep.sh
```

Outputs live under:

```text
benchmarks/corpora/vegeta-ethereum/s3/native-execution/compute-calibration-sweep/
```

The combined `summary.txt` reports, for every metric/scale, source/native rank correlation,
critical-chain native overweight, and replay scaling. The intended interpretation is diagnostic,
not threshold-seeking:

- correlation should rise as source-weighted compute dominates semantic-port fixed costs;
- `critical-overweight` should move toward `1.0` if source computational placement is restored;
- if `exact-direct` scaling rises with scale, the semantic-only workload was too fine-grained to
  amortize scheduler/runtime overhead;
- if fidelity improves but replay scaling stays flat, the remaining bottleneck is in the execution
  substrate rather than workload weighting;
- `net` speedup divides a strategy's active speedup by the same-profile serial-strategy control to
  reduce the paired-run order/cache bias observed in the S3 harness.

Two frozen S3 source traces are unavailable from the public RPC reconstruction. They receive zero
supplemental compute and remain explicitly reported; affected blocks are excluded from weighted
critical-path analysis rather than having source cost imputed.
