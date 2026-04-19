# Performance Testing

This guide describes the Node-side end-to-end and performance harness for
`wasmsh-pyodide`. The goal is to make startup and first-command regressions
measurable before tuning work starts.

## What the harness measures

The benchmark in [tools/perf/pyodide-node-session-bench.mjs](/Users/johann/src/ml/wasmsh/tools/perf/pyodide-node-session-bench.mjs)
runs the real packaged Node host and records a small set of phases for each
sample:

- `spawnMs`: child-process spawn and client setup
- `initMs`: runtime init, including Pyodide module boot and wasmsh init
- `firstShellMs`: first shell command on a fresh session
- `firstPythonMs`: first Python command on a fresh session
- `steadyStateMs`: a second Python command after the runtime is already hot
- `closeMs`: session shutdown
- `totalMs`: sum of the measured phases above

The harness writes raw samples and summary percentiles to JSON so tuning work
can compare before/after changes directly.

For both performance harnesses in this repo:

- `n` means the number of measured batches/samples
- `c` means the concurrency per batch
- total measured sessions = `n * c`

With `c > 1`, the report includes both per-session latency summaries and
per-batch throughput/wall-clock summaries.

The runner restore smoke benchmark in
[tools/runner-node/bench/restore-latency.test.mjs](/Users/johann/src/ml/wasmsh/tools/runner-node/bench/restore-latency.test.mjs)
measures the ready-pod restore path separately. It now supports:

- `WASMSH_RESTORE_BENCH_WARMUP` to discard extra warmup sessions
- `WASMSH_RESTORE_BENCH_ITERATIONS` to control measured samples
- `WASMSH_RESTORE_BENCH_CONCURRENCY` to run multiple restores in parallel
- `WASMSH_RESTORE_STRICT_MS` to turn the smoke into a threshold gate

The runner load benchmark in
[tools/perf/runner-restore-bench.mjs](/Users/johann/src/ml/wasmsh/tools/perf/runner-restore-bench.mjs)
also measures three post-restore command timings for each session:

- `shellStartMs`: first shell command on a freshly restored session
- `shellExecMs`: second simple shell command once the shell path is hot
- `pythonExecMs`: simple Python command on that same session

The scalable runner defaults its worker V8 envelope to:

- `WASMSH_WORKER_MAX_OLD_GENERATION_MB=48`
- `WASMSH_WORKER_MAX_YOUNG_GENERATION_MB=8`
- `WASMSH_WORKER_STACK_MB=1`
- `WASMSH_WORKER_CODE_RANGE_MB=8`

Those defaults are tuned for higher live-session density. Override them only
when you are intentionally trading memory for latency or debugging headroom.

## Run the benchmark locally

Build the Pyodide assets first:

```bash
just build-pyodide
```

Run the benchmark with defaults:

```bash
just bench-e2e-pyodide-node
```

The default output path is:

```text
artifacts/perf/pyodide-node-session-bench.json
```

Use a larger sample set when measuring an optimization:

```bash
node tools/perf/pyodide-node-session-bench.mjs --warmup 2 -n 10 -c 4
```

For the scalable runner restore path:

```bash
WASMSH_RESTORE_BENCH_WARMUP=2 WASMSH_RESTORE_BENCH_ITERATIONS=10 WASMSH_RESTORE_BENCH_CONCURRENCY=8 just bench-runner-restore
```

For an explicit runner load report with `n`/`c` flags:

```bash
node tools/perf/runner-restore-bench.mjs -n 10 -c 16
```

`just bench-runner-load` now defaults to `n=2000` and `c=100`, so the out-of-the-box run measures `200,000` total session restores and acts as a real per-runner load sample rather than a single-session smoke check.

To turn the benchmark into a gate, pass explicit limits:

```bash
node tools/perf/pyodide-node-session-bench.mjs \
  -n 10 \
  -c 4 \
  --max-init-p95-ms 1200 \
  --max-first-python-p95-ms 250 \
  --max-total-p95-ms 1800
```

For the runner restore bench:

```bash
node tools/perf/runner-restore-bench.mjs \
  -n 10 \
  -c 16 \
  --max-restore-p95-ms 100
```

No thresholds are enforced by default because the current purpose is to
establish a stable baseline for tuning.

## Run the e2e perf harness test

The perf harness itself has a small Node test that validates the report shape
against a real runtime:

```bash
node --test e2e/pyodide-node/tests/perf-harness.test.mjs
```

This is complementary to the existing functional suite in
`e2e/pyodide-node/tests/*.test.mjs`. The functional suite proves behavior;
the perf harness proves that the critical startup path is being measured in a
repeatable way.

## Interpreting the report

Each report includes:

- `measurements`: raw per-sample timings and command exit-code checks
- `metrics`: summary stats for each phase (`min`, `mean`, `p50`, `p95`, `max`)
- `batchMetrics`: wall-clock and throughput summaries for each measured batch
- `memory`: process baseline RSS, runner baseline RSS, peak RSS, post-close RSS, a run-wide average per-active-session RSS delta, and a peak-derived per-active-session RSS estimate
- `recordedAt`, `nodeExecutable`, and `assetDir`: enough context to compare runs

When tuning startup:

1. watch `initMs` first, because it captures almost all cold-session work
2. compare `firstPythonMs` to `steadyStateMs` to see whether Python import
   or post-init lazy work is leaking into the first request
3. use `totalMs` only as a coarse user-visible number; it is less useful than
   the phase breakdown when you are trying to decide what to optimize next

When tuning scalable runner capacity:

1. increase `c` until `restoreMs p95` or `batchCreateWallMs p95` starts bending up
2. watch `throughputSessionsPerSec` to find the useful saturation point
3. compare `workerSpawnMs` versus `sandboxRestoreMs` to see whether the bottleneck is worker startup or the snapshot replay path
4. compare `shellStartMs`, `shellExecMs`, and `pythonExecMs` to see whether post-restore latency is dominated by first-shell overhead, steady shell work, or Python startup
5. use `averagePerActiveSessionRssBytesWhenBusy` to see the average single-sandbox memory cost across the run, and `estimatedPerActiveSessionRssBytes` to size for peak pressure

## End-to-end dispatcher benchmark

The in-process harness above measures the runner in isolation. To
include the dispatcher HTTP hop and restore-capacity routing — what a
remote `WasmshRemoteSandbox` client actually sees — run:

```bash
just bench-dispatcher-compose           # build + compose up + bench + down
just bench-dispatcher-compose-reuse     # skip compose cycle (faster)
node e2e/dispatcher-compose/scripts/benchmark.mjs --help
```

The benchmark drives `WasmshRemoteSandbox` through four phases
(sequential session create, steady-state execute, concurrency sweep,
file round-trip) and appends the runner's `/runner/snapshot` and
Prometheus text to the JSON report so queue depth and restore p95 sit
alongside the client-side latencies.

## Sizing a scalable deployment

Order-of-magnitude numbers from a local compose run on a developer
laptop (Apple Silicon, single runner, 4 restore slots). Treat as rough
guidance; your workload will differ.

Per session (stock Pyodide + bash, no heavy Python packages):

| metric | value |
|-|-|
| settled RSS (amortised across concurrent sessions) | **~80 MB** |
| cold create (worker spawn bound) | **~300 ms** |
| snapshot restore once template is warm | **~6 ms** |
| warm bash command via dispatcher | **~1.5 ms** |
| warm `python3 -c` round-trip | **~3 ms** |

Projected density for a **64 GB / 40-core** node (~58 GB usable after
OS, dispatcher, and per-pod runner baselines):

| profile | per-session memory | concurrent active sessions |
|-|-|-|
| Lean (fresh sessions, `MAX_OLD=48`) | ~60 MB | **~950** |
| Typical agent | ~80 MB | **~700** |
| Heavy (pandas/numpy, large wasm heap) | ~250 MB | **~230** |

Cold-start throughput is CPU-bound (~300 ms per spawn on one core), so a
40-thread box sustains roughly **~100 session creates/s** once
`restoreSlots` is raised from the chart default of 4. Memory is the
binding constraint in steady state; CPU only hurts during cold-start
bursts, which the HPA on `wasmsh_inflight_restores` smooths out.

Practical starting point for a node of that shape: **8 runner pods × 8
restore slots**, target ~500–800 warm sessions.

Re-run `just bench-dispatcher-compose` on your own hardware — or better,
`just test-e2e-kind` against a production-parity Helm install — before
committing to a deployment size.
