import assert from "node:assert/strict";

import {
  assertRestoreThresholds,
  runRestoreBenchmark,
} from "./restore-benchmark.mjs";

const warmupIterations = Number(process.env.WASMSH_RESTORE_BENCH_WARMUP ?? 0);
const iterations = Number(process.env.WASMSH_RESTORE_BENCH_ITERATIONS ?? 3);
const concurrency = Number(process.env.WASMSH_RESTORE_BENCH_CONCURRENCY ?? 1);
const strictThresholdMs = process.env.WASMSH_RESTORE_STRICT_MS
  ? Number(process.env.WASMSH_RESTORE_STRICT_MS)
  : null;

const report = await runRestoreBenchmark({
  warmup: warmupIterations,
  samples: iterations,
  concurrency,
});

if (strictThresholdMs !== null) {
  assertRestoreThresholds(report, {
    restoreMs: strictThresholdMs,
  });
}

process.stdout.write(
  `${JSON.stringify({
    ok: true,
    warmupIterations,
    iterations,
    concurrency,
    totalSessions: report.totalSessions,
    p95: report.metrics.restoreMs.p95,
    stageP95: {
      worker_spawn: report.metrics.workerSpawnMs.p95,
      sandbox_restore: report.metrics.sandboxRestoreMs.p95,
      shell_start: report.metrics.shellStartMs.p95,
      shell_exec: report.metrics.shellExecMs.p95,
      python_exec: report.metrics.pythonExecMs.p95,
    },
    batchCreateWallP95: report.batchMetrics.batchCreateWallMs.p95,
    throughputP95: report.batchMetrics.throughputSessionsPerSec.p95,
  })}\n`,
);
