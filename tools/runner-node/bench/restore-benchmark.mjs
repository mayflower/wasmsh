import { performance } from "node:perf_hooks";

import { createRunner } from "../src/runner-main.mjs";

export const DEFAULT_RUNNER_BENCH_COMMANDS = {
  shellStart: "echo ready",
  shellExec: "pwd",
  pythonExec: "python3 -c \"print(42)\"",
};

function percentile(samples, quantile) {
  if (samples.length === 0) {
    return 0;
  }
  const sorted = [...samples].sort((left, right) => left - right);
  const index = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil(sorted.length * quantile) - 1),
  );
  return sorted[index];
}

function summarizeSamples(samples, field) {
  const values = samples
    .map((sample) => sample[field])
    .filter((value) => Number.isFinite(value));
  if (values.length === 0) {
    return {
      count: 0,
      min: 0,
      max: 0,
      mean: 0,
      p50: 0,
      p95: 0,
    };
  }
  const sorted = [...values].sort((left, right) => left - right);
  const total = values.reduce((sum, value) => sum + value, 0);
  return {
    count: values.length,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    mean: total / values.length,
    p50: percentile(sorted, 0.5),
    p95: percentile(sorted, 0.95),
  };
}

function buildMeasurementSummary(measurements) {
  return {
    restoreMs: summarizeSamples(measurements, "restoreMs"),
    workerSpawnMs: summarizeSamples(measurements, "workerSpawnMs"),
    sandboxRestoreMs: summarizeSamples(measurements, "sandboxRestoreMs"),
    shellStartMs: summarizeSamples(measurements, "shellStartMs"),
    shellExecMs: summarizeSamples(measurements, "shellExecMs"),
    pythonExecMs: summarizeSamples(measurements, "pythonExecMs"),
  };
}

function buildBatchSummary(batches) {
  return {
    batchCreateWallMs: summarizeSamples(batches, "batchCreateWallMs"),
    batchCloseWallMs: summarizeSamples(batches, "batchCloseWallMs"),
    throughputSessionsPerSec: summarizeSamples(batches, "throughputSessionsPerSec"),
  };
}

function buildMemorySummary({
  processBaselineRssBytes,
  runnerBaselineRssBytes,
  peakRssBytes,
  finalRssBytes,
  concurrency,
  samplesTotal,
  activeSamplesTotal,
  sumRssBytes,
  sumActiveSessions,
  sumRssBytesWhenBusy,
  sumAboveRunnerBaselineRssBytesWhenBusy,
  sumPerActiveSessionRssBytesWhenBusy,
}) {
  const peakAboveRunnerBaseline = Math.max(0, peakRssBytes - runnerBaselineRssBytes);
  const averageRssBytes = samplesTotal > 0 ? sumRssBytes / samplesTotal : 0;
  const averageActiveSessions = samplesTotal > 0 ? sumActiveSessions / samplesTotal : 0;
  const averageRssBytesWhenBusy = activeSamplesTotal > 0 ? sumRssBytesWhenBusy / activeSamplesTotal : 0;
  const averageAboveRunnerBaselineRssBytesWhenBusy =
    activeSamplesTotal > 0 ? sumAboveRunnerBaselineRssBytesWhenBusy / activeSamplesTotal : 0;
  const averagePerActiveSessionRssBytesWhenBusy =
    activeSamplesTotal > 0 ? sumPerActiveSessionRssBytesWhenBusy / activeSamplesTotal : 0;
  return {
    processBaselineRssBytes,
    runnerBaselineRssBytes,
    peakRssBytes,
    finalRssBytes,
    retainedRssBytes: Math.max(0, finalRssBytes - processBaselineRssBytes),
    averageRssBytes,
    averageActiveSessions,
    averageRssBytesWhenBusy,
    averageAboveRunnerBaselineRssBytesWhenBusy,
    averagePerActiveSessionRssBytesWhenBusy,
    peakAboveRunnerBaselineRssBytes: peakAboveRunnerBaseline,
    estimatedPerActiveSessionRssBytes: peakAboveRunnerBaseline / Math.max(1, concurrency),
  };
}

async function timeAsync(fn) {
  const started = performance.now();
  const result = await fn();
  return {
    durationMs: performance.now() - started,
    result,
  };
}

function assertSuccessfulRun(result, label) {
  if ((result?.exitCode ?? 1) !== 0) {
    throw new Error(`${label} failed with exitCode=${result?.exitCode ?? "unknown"}`);
  }
}

async function collectRestoreBatch(runner, options = {}, batchIndex = 0) {
  const concurrency = Math.max(1, options.concurrency ?? 1);
  const commands = {
    ...DEFAULT_RUNNER_BENCH_COMMANDS,
    ...(options.commands ?? {}),
  };
  const createStarted = performance.now();
  const created = await Promise.all(
    Array.from({ length: concurrency }, async (_, slot) => {
      const session = await runner.createSession();
      return {
        batchIndex,
        slot,
        session,
        restoreMs: session.restoreMetrics.total,
        workerSpawnMs: session.restoreMetrics.stages.worker_spawn ?? 0,
        sandboxRestoreMs: session.restoreMetrics.stages.sandbox_restore ?? 0,
      };
    }),
  );
  const batchCreateWallMs = performance.now() - createStarted;

  const measured = await Promise.all(
    created.map(async (measurement) => {
      const shellStart = await timeAsync(() => measurement.session.run(commands.shellStart));
      assertSuccessfulRun(shellStart.result, "shellStart");

      const shellExec = await timeAsync(() => measurement.session.run(commands.shellExec));
      assertSuccessfulRun(shellExec.result, "shellExec");

      const pythonExec = await timeAsync(() => measurement.session.run(commands.pythonExec));
      assertSuccessfulRun(pythonExec.result, "pythonExec");

      return {
        ...measurement,
        shellStartMs: shellStart.durationMs,
        shellExecMs: shellExec.durationMs,
        pythonExecMs: pythonExec.durationMs,
        checks: {
          shellStartExitCode: shellStart.result?.exitCode ?? null,
          shellExecExitCode: shellExec.result?.exitCode ?? null,
          pythonExecExitCode: pythonExec.result?.exitCode ?? null,
          shellStartStdout: shellStart.result?.stdout ?? "",
          shellExecStdout: shellExec.result?.stdout ?? "",
          pythonExecStdout: pythonExec.result?.stdout ?? "",
        },
      };
    }),
  );

  const closeStarted = performance.now();
  await Promise.all(measured.map(({ session }) => session.close()));
  const batchCloseWallMs = performance.now() - closeStarted;

  return {
    batchIndex,
    batchCreateWallMs,
    batchCloseWallMs,
    throughputSessionsPerSec: (concurrency * 1000) / Math.max(batchCreateWallMs, 1),
    measurements: measured.map(({ session, ...measurement }) => measurement),
  };
}

export async function runRestoreBenchmark(options = {}) {
  const warmup = Math.max(0, options.warmup ?? 0);
  const samples = Math.max(1, options.samples ?? 3);
  const concurrency = Math.max(1, options.concurrency ?? 1);
  const memorySampleIntervalMs = Math.max(5, options.memorySampleIntervalMs ?? 20);
  const commands = {
    ...DEFAULT_RUNNER_BENCH_COMMANDS,
    ...(options.commands ?? {}),
  };
  const processBaselineRssBytes = process.memoryUsage().rss;
  let peakRssBytes = processBaselineRssBytes;
  let samplesTotal = 0;
  let activeSamplesTotal = 0;
  let sumRssBytes = 0;
  let sumActiveSessions = 0;
  let sumRssBytesWhenBusy = 0;
  let sumAboveRunnerBaselineRssBytesWhenBusy = 0;
  let sumPerActiveSessionRssBytesWhenBusy = 0;
  const sampleRss = () => {
    const rss = process.memoryUsage().rss;
    peakRssBytes = Math.max(peakRssBytes, rss);
    if (runner) {
      const snapshot = runner.metrics.snapshot();
      const activeSessions =
        (snapshot.wasmsh_active_sessions ?? 0) + (snapshot.wasmsh_inflight_restores ?? 0);
      samplesTotal += 1;
      sumRssBytes += rss;
      sumActiveSessions += activeSessions;
      if (activeSessions > 0) {
        const aboveRunnerBaseline = Math.max(0, rss - runnerBaselineRssBytes);
        activeSamplesTotal += 1;
        sumRssBytesWhenBusy += rss;
        sumAboveRunnerBaselineRssBytesWhenBusy += aboveRunnerBaseline;
        sumPerActiveSessionRssBytesWhenBusy += aboveRunnerBaseline / activeSessions;
      }
    }
    return rss;
  };
  const memorySampler = setInterval(sampleRss, memorySampleIntervalMs);
  memorySampler.unref?.();
  let runnerBaselineRssBytes = processBaselineRssBytes;
  let finalRssBytes = processBaselineRssBytes;
  let runner;
  let report;
  try {
    runner = await createRunner(options.runnerOptions ?? {});
    runnerBaselineRssBytes = sampleRss();

    for (let index = 0; index < warmup; index += 1) {
      await collectRestoreBatch(runner, { concurrency, commands }, index);
    }

    const batches = [];
    for (let index = 0; index < samples; index += 1) {
      batches.push(await collectRestoreBatch(runner, { concurrency, commands }, index));
    }

    const measurements = batches.flatMap((batch) => batch.measurements);
    report = {
      suite: "runner-restore-benchmark",
      warmup,
      samples,
      concurrency,
      totalSessions: measurements.length,
      recordedAt: new Date().toISOString(),
      runnerSnapshot: runner.runnerSnapshot(),
      commands,
      metrics: buildMeasurementSummary(measurements),
      batchMetrics: buildBatchSummary(batches),
      batches,
      measurements,
    };
  } finally {
    clearInterval(memorySampler);
    if (runner) {
      await runner.close();
    }
    finalRssBytes = sampleRss();
  }
  return {
    ...report,
    memory: buildMemorySummary({
      processBaselineRssBytes,
      runnerBaselineRssBytes,
      peakRssBytes,
      finalRssBytes,
      concurrency,
      samplesTotal,
      activeSamplesTotal,
      sumRssBytes,
      sumActiveSessions,
      sumRssBytesWhenBusy,
      sumAboveRunnerBaselineRssBytesWhenBusy,
      sumPerActiveSessionRssBytesWhenBusy,
    }),
  };
}

export function assertRestoreThresholds(report, thresholds = {}) {
  const failures = [];
  for (const [metric, limit] of Object.entries(thresholds)) {
    if (!Number.isFinite(limit)) {
      continue;
    }
    const actual = report.metrics?.[metric]?.p95;
    if (!Number.isFinite(actual)) {
      failures.push(`missing metric '${metric}'`);
      continue;
    }
    if (actual > limit) {
      failures.push(`${metric} p95 ${actual.toFixed(2)}ms exceeded ${limit.toFixed(2)}ms`);
    }
  }
  if (failures.length > 0) {
    throw new Error(failures.join("; "));
  }
}
