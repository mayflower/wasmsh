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
node tools/perf/pyodide-node-session-bench.mjs --warmup 2 --samples 10
```

To turn the benchmark into a gate, pass explicit limits:

```bash
node tools/perf/pyodide-node-session-bench.mjs \
  --samples 10 \
  --max-init-p95-ms 1200 \
  --max-first-python-p95-ms 250 \
  --max-total-p95-ms 1800
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
- `recordedAt`, `nodeExecutable`, and `assetDir`: enough context to compare runs

When tuning startup:

1. watch `initMs` first, because it captures almost all cold-session work
2. compare `firstPythonMs` to `steadyStateMs` to see whether Python import
   or post-init lazy work is leaking into the first request
3. use `totalMs` only as a coarse user-visible number; it is less useful than
   the phase breakdown when you are trying to decide what to optimize next
