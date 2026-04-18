# Missing Perf Tuning

The repo now has a real measurement loop for `wasmsh-pyodide` startup:

- functional Node e2e coverage in [e2e/pyodide-node/tests](/Users/johann/src/ml/wasmsh/e2e/pyodide-node/tests)
- a phase-oriented benchmark in [tools/perf/pyodide-node-session-bench.mjs](/Users/johann/src/ml/wasmsh/tools/perf/pyodide-node-session-bench.mjs)
- usage guidance in [docs/guides/performance-testing.md](/Users/johann/src/ml/wasmsh/docs/guides/performance-testing.md)

What is still missing is the actual tuning pass. The likely next targets are:

1. Reduce `initMs` by removing unnecessary work from the boot path, especially
   package preloads and any network-related setup that is not needed for the
   baseline session.
2. Reduce `firstPythonMs` by identifying imports or bridge setup that can move
   into `init` or be cached across commands.
3. Add threshold values only after a few benchmark reports exist for the same
   machine class; otherwise the gate will be noise instead of signal.
4. Extend the benchmark to a pod-level or runner-process-level harness once the
   runtime architecture grows beyond the current direct Node host model.

The important change is that performance work can now be done against measured
phase timings instead of anecdotal startup impressions.
