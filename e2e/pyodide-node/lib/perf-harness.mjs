import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { performance } from "node:perf_hooks";

import { createNodeHostSession } from "../../../packages/npm/wasmsh-pyodide/lib/node-host-session.mjs";
import { resolveAssetPath, resolveNodeHostPath } from "../../../packages/npm/wasmsh-pyodide/index.js";

export const DEFAULT_PERF_COMMANDS = {
  shell: "echo ready",
  python: "python3 -c \"print(6 * 7)\"",
  steadyState: "python3 -c \"print(sum(range(1000)))\"",
};

function percentile(sortedValues, q) {
  if (sortedValues.length === 0) {
    return 0;
  }
  const index = Math.min(
    sortedValues.length - 1,
    Math.max(0, Math.ceil(sortedValues.length * q) - 1),
  );
  return sortedValues[index];
}

export function summarizeSamples(samples, field) {
  const values = samples.map((sample) => sample[field]).filter((value) => Number.isFinite(value));
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
  const sorted = [...values].sort((a, b) => a - b);
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

export function buildSummary(samples) {
  return {
    spawnMs: summarizeSamples(samples, "spawnMs"),
    initMs: summarizeSamples(samples, "initMs"),
    firstShellMs: summarizeSamples(samples, "firstShellMs"),
    firstPythonMs: summarizeSamples(samples, "firstPythonMs"),
    steadyStateMs: summarizeSamples(samples, "steadyStateMs"),
    closeMs: summarizeSamples(samples, "closeMs"),
    totalMs: summarizeSamples(samples, "totalMs"),
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

export async function collectSessionSample(options = {}) {
  const assetDir = options.assetDir ?? resolveAssetPath();
  const hostPath = options.hostPath ?? resolveNodeHostPath();
  const nodeExecutable = options.nodeExecutable ?? process.execPath;
  const timeoutMs = options.timeoutMs;
  const initOptions = options.initOptions ?? {};
  const commands = {
    ...DEFAULT_PERF_COMMANDS,
    ...(options.commands ?? {}),
  };

  const spawnStart = performance.now();
  const session = await createNodeHostSession({
    assetDir,
    hostPath,
    nodeExecutable,
    timeoutMs,
    autoInit: false,
  });
  const spawnMs = performance.now() - spawnStart;

  let initResult;
  let shellResult;
  let pythonResult;
  let steadyStateResult;
  let closeMs = 0;

  try {
    const initTiming = await timeAsync(() => session.init(initOptions));
    initResult = initTiming.result;

    const shellTiming = await timeAsync(() => session.run(commands.shell));
    shellResult = shellTiming.result;

    const pythonTiming = await timeAsync(() => session.run(commands.python));
    pythonResult = pythonTiming.result;

    const steadyStateTiming = await timeAsync(() => session.run(commands.steadyState));
    steadyStateResult = steadyStateTiming.result;

    const closeTiming = await timeAsync(() => session.close());
    closeMs = closeTiming.durationMs;

    return {
      spawnMs,
      initMs: initTiming.durationMs,
      firstShellMs: shellTiming.durationMs,
      firstPythonMs: pythonTiming.durationMs,
      steadyStateMs: steadyStateTiming.durationMs,
      closeMs,
      totalMs:
        spawnMs +
        initTiming.durationMs +
        shellTiming.durationMs +
        pythonTiming.durationMs +
        steadyStateTiming.durationMs +
        closeMs,
      checks: {
        initEventCount: Array.isArray(initResult?.events) ? initResult.events.length : 0,
        shellExitCode: shellResult?.exitCode ?? null,
        pythonExitCode: pythonResult?.exitCode ?? null,
        steadyStateExitCode: steadyStateResult?.exitCode ?? null,
        shellStdout: shellResult?.stdout ?? "",
        pythonStdout: pythonResult?.stdout ?? "",
        steadyStateStdout: steadyStateResult?.stdout ?? "",
      },
    };
  } catch (error) {
    try {
      await session.close();
    } catch {
      // Ignore cleanup errors and preserve the original failure.
    }
    throw error;
  }
}

export async function runSessionBenchmark(options = {}) {
  const warmup = options.warmup ?? 1;
  const samples = options.samples ?? 5;

  for (let i = 0; i < warmup; i += 1) {
    await collectSessionSample(options);
  }

  const measurements = [];
  for (let i = 0; i < samples; i += 1) {
    measurements.push(await collectSessionSample(options));
  }

  return {
    suite: "pyodide-node-session",
    samples,
    warmup,
    nodeExecutable: options.nodeExecutable ?? process.execPath,
    assetDir: options.assetDir ?? resolveAssetPath(),
    commands: {
      ...DEFAULT_PERF_COMMANDS,
      ...(options.commands ?? {}),
    },
    recordedAt: new Date().toISOString(),
    metrics: buildSummary(measurements),
    measurements,
  };
}

export function writeBenchmarkReport(report, outputPath) {
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`, "utf-8");
  return outputPath;
}

export function defaultReportPath(filename = "pyodide-node-session-bench.json") {
  return resolve(process.cwd(), "artifacts", "perf", filename);
}

export function assertThresholds(report, thresholds = {}) {
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
