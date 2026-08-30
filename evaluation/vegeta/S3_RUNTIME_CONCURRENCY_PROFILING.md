# Vegeta S3 runtime concurrency profiling

This diagnostic is intentionally narrower than the publication scheduler sweep. It profiles only
three workload shapes that bracket the behavior observed so far:

- `none / 0x`: semantic-only native workload;
- `steps / 4x`: the most source-faithful calibrated workload from the prior sweep;
- `gas / 4x`: a heavier parallelism stress profile.

For each profile it runs `serial`, `exact-direct`, and `exact-access` at 1, 2, 4, 8, and 16
workers. The benchmark is invoked with `--runtime-profile`, which adds aggregate hot-path counters.
These profiling runs should not replace the uninstrumented performance sweep when quoting final
speedups.

## What is measured

`exact-direct` reports:

- worker-phase wall time;
- aggregate worker READY-queue wait;
- aggregate transaction service time;
- maximum in-flight transactions;
- canonical world read-lock wait and hold time;
- batched canonical commit writer-lock wait and hold time;
- Wasm instance acquire/reuse, entrypoint, host-storage/query, and transaction-lock timings.

`exact-access` reuses the engine's dependency/MVCC diagnostics and reports:

- worker-phase wall and aggregate READY wait;
- visibility-capture and publish/unblock time;
- aggregate contract execution;
- Wasm acquire/reuse and entrypoint time;
- host-storage/query time;
- transaction-lock and MVCC-lock/publish time;
- maximum in-flight transactions.

Worker-time fields can exceed wall time because workers overlap. Nested contract fields are not
additive: for example, host storage callbacks happen inside Wasm entrypoint/request time.

## Running the profiler

```bash
bash tools/legacy-scripts/run-vegeta-s3-runtime-concurrency-profile.sh
```

Outputs are written by default to:

```text
benchmarks/corpora/vegeta-ethereum/s3/native-execution/runtime-concurrency-profile/
```

The main report is:

```text
runtime-concurrency-profile/summary.txt
```

Useful environment overrides:

```bash
VEGETA_S3_PROFILE_WORKERS=1,2,4,8,16
VEGETA_S3_PROFILE_SAMPLES=1
VEGETA_S3_PROFILE_PERF=auto        # auto | on | off
VEGETA_S3_PROFILE_PERF_STRATEGY=exact-direct
VEGETA_S3_PERF_BIN=/path/to/perf
```

The internal diagnostics run even if `perf` is unavailable. In `auto` mode the script enables
`perf stat` when it can execute `task-clock`; otherwise it prints a warning and continues.

## Installing `perf` on Ubuntu under WSL2

First make sure WSL itself is current. From **Windows PowerShell**:

```powershell
wsl --update
wsl --shutdown
```

Then reopen the Ubuntu WSL terminal.

### Fast path: Ubuntu packages

Start with:

```bash
sudo apt update
sudo apt install -y linux-tools-common linux-tools-generic
```

Check:

```bash
perf --version
```

On WSL, Ubuntu's `/usr/bin/perf` wrapper can complain that a package matching the exact
`*-microsoft-standard-WSL2` kernel name is unavailable even though a usable `perf` binary was
installed. If that happens, locate and use the real binary directly:

```bash
PERF_BIN="$(find /usr/lib/linux-tools -type f -name perf -perm -111 2>/dev/null | sort -V | tail -n1)"
"$PERF_BIN" --version
export VEGETA_S3_PERF_BIN="$PERF_BIN"
```

Then verify that at least software counters work:

```bash
"$VEGETA_S3_PERF_BIN" stat -e task-clock,context-switches,cpu-migrations,page-faults -- true
```

If `perf` reports a permissions error and this is your own development WSL instance, inspect:

```bash
cat /proc/sys/kernel/perf_event_paranoid
```

A temporary, less restrictive setting for the current WSL boot is:

```bash
sudo sysctl -w kernel.perf_event_paranoid=1
```

Retry the `perf stat` command after changing it. Do not lower the setting further unless you need
additional counters and understand the security trade-off.

### Fallback: build `perf` from Microsoft's WSL kernel source

If Ubuntu packages do not provide a usable binary, install build dependencies:

```bash
sudo apt update
sudo apt install -y \
  build-essential flex bison dwarves libssl-dev libelf-dev cpio qemu-utils rsync git
```

Then:

```bash
git clone --depth 1 https://github.com/microsoft/WSL2-Linux-Kernel.git
cd WSL2-Linux-Kernel
make -C tools/perf \
  NO_JEVENTS=1 \
  NO_JVMTI=1 \
  NO_LIBTRACEEVENT=1 \
  install DESTDIR="$PWD/perf-install" prefix=/usr/local
sudo cp "$PWD/perf-install/usr/local/bin/perf" /usr/local/bin/perf-wsl
/usr/local/bin/perf-wsl --version
export VEGETA_S3_PERF_BIN=/usr/local/bin/perf-wsl
```

Microsoft's WSL2 kernel source tree documents building `tools/perf` this way. WSL kernel support for
specific hardware PMU counters still depends on the host, WSL version, kernel configuration, and
virtualization path. The profiling script therefore probes the requested events: it uses
hardware+software counters when available and falls back to software counters otherwise.

## Reading the output

The most diagnostic fields are:

- `service-conc`: aggregate transaction service time / worker-phase wall. If 8 workers are
  configured but this remains around 2-3, the execution substrate is not keeping the workers busy.
- `ready-cap`: aggregate READY-queue wait / total worker capacity. High values mean workers are
  starved by DAG/scheduler readiness rather than executing contracts.
- `canon-read-wait`: canonical-state reader lock wait relative to request execution.
- `commit-hold`: canonical batched commit lock hold relative to strategy wall time.
- `wasm acquire/request`, `entrypoint/request`, `host-storage/request`, and `mvcc-lock/request`:
  identify which nested runtime layers dominate request time.
- `perf-cpus`: `task-clock / process elapsed`. Roughly `4.0` means four CPUs were busy on average
  across the profiled process. The `perf` process includes benchmark setup and the matched serial
  control, so use this as a corroborating system-level signal rather than a pure target-strategy CPU
  utilization number.

A useful interpretation matrix is:

- low `service-conc` + high `ready-cap`: dependency/scheduler starvation;
- low `service-conc` + low `ready-cap` + high canonical/MVCC lock wait: shared-state contention;
- high `service-conc` but weak wall-clock speedup + high cache-miss rate: memory/cache pressure or
  duplicated runtime work;
- `perf-cpus` saturating near the machine's physical CPU allowance: hardware/WSL CPU ceiling;
- Wasm acquire dominating request time despite `Reuse`: VM cache/lifecycle implementation issue.
