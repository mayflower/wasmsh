#!/usr/bin/env node
// Performance benchmark for the scalable (dispatcher + runner) setup.
//
// Drives `WasmshRemoteSandbox` end-to-end through the docker-compose stack
// defined in deploy/docker/compose.dispatcher-test.yml (same stack the
// dispatcher-compose e2e uses).  Measures what a remote caller actually
// sees on the wire: session create, execute, concurrent throughput, and
// file upload/download round-trip.  After the client-side phases finish,
// the Prometheus snapshot from the runner is scraped via
// `docker compose exec` so the report also includes queue depth, restore
// p95, and active sessions.
//
// Usage:
//   node e2e/dispatcher-compose/scripts/benchmark.mjs             # full cycle
//   node e2e/dispatcher-compose/scripts/benchmark.mjs --keep      # leave stack up
//   node e2e/dispatcher-compose/scripts/benchmark.mjs --reuse     # assume stack running
//   node e2e/dispatcher-compose/scripts/benchmark.mjs --skip-build
//   node e2e/dispatcher-compose/scripts/benchmark.mjs --sessions 30 --concurrency 1,4,8,16
//   node e2e/dispatcher-compose/scripts/benchmark.mjs --report /tmp/bench.json
import { performance } from "node:perf_hooks";
import { writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

import {
  commandExists,
  runCommand,
} from "../../kind/lib/process.mjs";
import {
  buildImages,
  DISPATCHER_IMAGE,
  REPO_ROOT,
  RUNNER_IMAGE,
} from "../../kind/lib/cluster.mjs";
import { createDocker } from "../../kind/lib/docker.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const COMPOSE_FILE = resolve(
  REPO_ROOT,
  "deploy/docker/compose.dispatcher-test.yml",
);
const DEFAULT_DISPATCHER_URL = "http://localhost:8080";

function parseIntList(value, fallback) {
  if (!value) return fallback;
  return value
    .split(",")
    .map((part) => Number.parseInt(part.trim(), 10))
    .filter((n) => Number.isFinite(n) && n > 0);
}

function percentile(sortedValues, q) {
  if (sortedValues.length === 0) return 0;
  const index = Math.min(
    sortedValues.length - 1,
    Math.max(0, Math.ceil(sortedValues.length * q) - 1),
  );
  return sortedValues[index];
}

function summarise(values) {
  if (values.length === 0) {
    return { count: 0, min: 0, p50: 0, p95: 0, p99: 0, max: 0, mean: 0 };
  }
  const sorted = [...values].sort((a, b) => a - b);
  const total = sorted.reduce((sum, v) => sum + v, 0);
  return {
    count: sorted.length,
    min: sorted[0],
    p50: percentile(sorted, 0.5),
    p95: percentile(sorted, 0.95),
    p99: percentile(sorted, 0.99),
    max: sorted[sorted.length - 1],
    mean: total / sorted.length,
  };
}

function formatMs(n) {
  return Number.isFinite(n) ? `${n.toFixed(1)}ms` : "n/a";
}

function formatRate(n, unit) {
  return Number.isFinite(n) ? `${n.toFixed(2)} ${unit}` : "n/a";
}

async function measure(fn) {
  const started = performance.now();
  const result = await fn();
  return { durationMs: performance.now() - started, result };
}

// Dispatcher rejects create_session with "no healthy runner has free
// restore capacity" once all restore slots are busy.  A real client retries
// with backoff; the benchmark does the same so concurrency > slot_count
// measures queueing latency instead of crashing.
const CAPACITY_ERROR_PATTERN = /no healthy runner has free restore capacity/i;

async function createSandboxWithBackoff(dispatcherUrl, { maxAttempts = 40, delayMs = 100 } = {}) {
  let lastError;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      return await WasmshRemoteSandbox.create({ dispatcherUrl });
    } catch (error) {
      lastError = error;
      if (!CAPACITY_ERROR_PATTERN.test(error?.message ?? "")) {
        throw error;
      }
      await new Promise((r) => setTimeout(r, delayMs));
    }
  }
  throw lastError ?? new Error("createSandboxWithBackoff exhausted");
}

async function waitForReadyz(dispatcherUrl, timeoutMs = 120_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const resp = await fetch(`${dispatcherUrl}/readyz`);
      if (resp.ok) {
        const body = await resp.json().catch(() => ({}));
        return body;
      }
    } catch {
      // retry
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`dispatcher not ready within ${timeoutMs}ms at ${dispatcherUrl}/readyz`);
}

async function composeUp() {
  await runCommand(
    "docker",
    [
      "compose",
      "-f",
      COMPOSE_FILE,
      "up",
      "-d",
      "--wait",
      "--wait-timeout",
      "240",
    ],
    { inherit: true, timeoutMs: 10 * 60 * 1000 },
  );
}

async function composeDown() {
  await runCommand(
    "docker",
    ["compose", "-f", COMPOSE_FILE, "down", "--remove-orphans"],
    { inherit: true, timeoutMs: 2 * 60 * 1000 },
  );
}

async function retagImages() {
  const docker = createDocker();
  await docker.run(["tag", DISPATCHER_IMAGE, "ghcr.io/mayflower/wasmsh-dispatcher:latest"]);
  await docker.run(["tag", RUNNER_IMAGE, "ghcr.io/mayflower/wasmsh-runner:latest"]);
}

// Scrape the runner's in-container /runner/snapshot endpoint.  The runner's
// port is not published to the host in compose.dispatcher-test.yml, so go
// through `docker compose exec` -> node one-liner.
async function runnerSnapshot() {
  try {
    const { stdout } = await runCommand(
      "docker",
      [
        "compose",
        "-f",
        COMPOSE_FILE,
        "exec",
        "-T",
        "runner",
        "node",
        "-e",
        "require('http').get('http://127.0.0.1:8787/runner/snapshot',r=>{let d='';r.on('data',c=>d+=c);r.on('end',()=>{process.stdout.write(d)})}).on('error',e=>{console.error(e.message);process.exit(1)})",
      ],
      { timeoutMs: 10_000 },
    );
    return JSON.parse(stdout);
  } catch (error) {
    return { error: error.message };
  }
}

async function runnerPrometheusText() {
  try {
    const { stdout } = await runCommand(
      "docker",
      [
        "compose",
        "-f",
        COMPOSE_FILE,
        "exec",
        "-T",
        "runner",
        "node",
        "-e",
        "require('http').get('http://127.0.0.1:8787/metrics',r=>{let d='';r.on('data',c=>d+=c);r.on('end',()=>{process.stdout.write(d)})}).on('error',e=>{console.error(e.message);process.exit(1)})",
      ],
      { timeoutMs: 10_000 },
    );
    return stdout;
  } catch (error) {
    return `# error: ${error.message}`;
  }
}

async function phaseSessionCreate(dispatcherUrl, count) {
  const latencies = [];
  const startStage = [];
  const sandboxes = [];
  try {
    for (let i = 0; i < count; i += 1) {
      const { durationMs, result } = await measure(() =>
        createSandboxWithBackoff(dispatcherUrl),
      );
      latencies.push(durationMs);
      sandboxes.push(result);
      const probe = await measure(() => result.execute("echo ready"));
      startStage.push(probe.durationMs);
    }
    return {
      count,
      createMs: summarise(latencies),
      firstExecMs: summarise(startStage),
    };
  } finally {
    await Promise.all(sandboxes.map((s) => s.stop().catch(() => {})));
  }
}

async function phaseExecuteLatency(dispatcherUrl, commands, repeats) {
  const sandbox = await createSandboxWithBackoff(dispatcherUrl);
  try {
    const perCommand = {};
    for (const [label, command] of Object.entries(commands)) {
      const samples = [];
      for (let i = 0; i < repeats; i += 1) {
        const { durationMs, result } = await measure(() => sandbox.execute(command));
        if (result.exitCode !== 0) {
          throw new Error(
            `execute ${label} (${command}) failed with exit=${result.exitCode} output=${JSON.stringify(result.output)}`,
          );
        }
        samples.push(durationMs);
      }
      perCommand[label] = summarise(samples);
    }
    return { repeats, commands: perCommand };
  } finally {
    await sandbox.stop().catch(() => {});
  }
}

async function phaseConcurrentSessions(dispatcherUrl, concurrency, perSandboxRuns) {
  const wallStart = performance.now();
  const sandboxes = [];
  const createLatencies = [];
  const firstExecLatencies = [];
  const execLatencies = [];
  try {
    const created = await Promise.all(
      Array.from({ length: concurrency }, async () => {
        const { durationMs, result } = await measure(() =>
          createSandboxWithBackoff(dispatcherUrl),
        );
        createLatencies.push(durationMs);
        sandboxes.push(result);
        return result;
      }),
    );

    await Promise.all(
      created.map(async (sandbox) => {
        const first = await measure(() => sandbox.execute("echo ready"));
        firstExecLatencies.push(first.durationMs);
        for (let i = 1; i < perSandboxRuns; i += 1) {
          const { durationMs, result } = await measure(() => sandbox.execute("pwd"));
          if (result.exitCode !== 0) {
            throw new Error(`execute pwd failed exit=${result.exitCode}`);
          }
          execLatencies.push(durationMs);
        }
      }),
    );

    const totalOps = concurrency * perSandboxRuns;
    const wallMs = performance.now() - wallStart;
    return {
      concurrency,
      perSandboxRuns,
      totalOps,
      wallMs,
      opsPerSec: (totalOps * 1000) / Math.max(wallMs, 1),
      sessionsPerSec: (concurrency * 1000) / Math.max(wallMs, 1),
      createMs: summarise(createLatencies),
      firstExecMs: summarise(firstExecLatencies),
      steadyExecMs: summarise(execLatencies),
    };
  } finally {
    await Promise.all(sandboxes.map((s) => s.stop().catch(() => {})));
  }
}

async function phaseFileRoundTrip(dispatcherUrl, sizes) {
  const sandbox = await createSandboxWithBackoff(dispatcherUrl);
  try {
    const results = {};
    for (const sizeBytes of sizes) {
      const payload = new Uint8Array(sizeBytes);
      for (let i = 0; i < payload.length; i += 1) {
        payload[i] = (i * 31 + 7) & 0xff;
      }
      const path = `/workspace/bench-${sizeBytes}.bin`;
      const up = await measure(() => sandbox.uploadFiles([[path, payload]]));
      if (up.result[0]?.error) {
        throw new Error(`upload ${sizeBytes} failed: ${up.result[0].error}`);
      }
      const down = await measure(() => sandbox.downloadFiles([path]));
      if (down.result[0]?.error) {
        throw new Error(`download ${sizeBytes} failed: ${down.result[0].error}`);
      }
      if (down.result[0].content?.length !== sizeBytes) {
        throw new Error(`download ${sizeBytes} length mismatch: ${down.result[0].content?.length}`);
      }
      results[`${sizeBytes}B`] = {
        sizeBytes,
        uploadMs: up.durationMs,
        downloadMs: down.durationMs,
        uploadMBps: sizeBytes / 1_048_576 / (up.durationMs / 1000),
        downloadMBps: sizeBytes / 1_048_576 / (down.durationMs / 1000),
      };
    }
    return results;
  } finally {
    await sandbox.stop().catch(() => {});
  }
}

function renderSummary(report) {
  const lines = [];
  lines.push("");
  lines.push("=== wasmsh scalable benchmark ===");
  lines.push(`dispatcher:            ${report.dispatcherUrl}`);
  lines.push(`runners healthy:       ${report.readyz.healthy_runners ?? "?"}`);
  lines.push("");

  lines.push("-- session create (sequential) --");
  const create = report.phases.sessionCreate.createMs;
  lines.push(
    `n=${report.phases.sessionCreate.count}  p50=${formatMs(create.p50)}  p95=${formatMs(create.p95)}  p99=${formatMs(create.p99)}  max=${formatMs(create.max)}`,
  );
  const firstExec = report.phases.sessionCreate.firstExecMs;
  lines.push(
    `  first exec:  p50=${formatMs(firstExec.p50)}  p95=${formatMs(firstExec.p95)}  max=${formatMs(firstExec.max)}`,
  );
  lines.push("");

  lines.push("-- execute latency (reused session) --");
  for (const [label, stats] of Object.entries(report.phases.executeLatency.commands)) {
    lines.push(
      `${label.padEnd(14)} p50=${formatMs(stats.p50)}  p95=${formatMs(stats.p95)}  max=${formatMs(stats.max)}`,
    );
  }
  lines.push("");

  lines.push("-- concurrent sessions --");
  for (const run of report.phases.throughput) {
    lines.push(
      `c=${String(run.concurrency).padStart(3)}  wall=${formatMs(run.wallMs)}  sessions/s=${formatRate(run.sessionsPerSec, "sess/s")}  ops/s=${formatRate(run.opsPerSec, "ops/s")}`,
    );
    lines.push(
      `         create p95=${formatMs(run.createMs.p95)}  first-exec p95=${formatMs(run.firstExecMs.p95)}  steady-exec p95=${formatMs(run.steadyExecMs.p95)}`,
    );
  }
  lines.push("");

  lines.push("-- file round-trip --");
  for (const [label, stats] of Object.entries(report.phases.fileRoundTrip)) {
    lines.push(
      `${label.padEnd(10)} upload=${formatMs(stats.uploadMs)} (${formatRate(stats.uploadMBps, "MB/s")})  download=${formatMs(stats.downloadMs)} (${formatRate(stats.downloadMBps, "MB/s")})`,
    );
  }
  lines.push("");

  const snap = report.runnerSnapshot?.runner;
  if (snap) {
    lines.push("-- runner snapshot (post-bench) --");
    lines.push(
      `active=${snap.active_sessions ?? "?"}  inflight=${snap.inflight_restores ?? "?"}  queue=${snap.restore_queue_depth ?? "?"}  restore_p95=${formatMs(snap.restore_p95_ms ?? Number.NaN)}`,
    );
    lines.push("");
  }

  return lines.join("\n");
}

async function main() {
  const { values } = parseArgs({
    options: {
      keep: { type: "boolean", default: false },
      reuse: { type: "boolean", default: false },
      "skip-build": { type: "boolean", default: false },
      sessions: { type: "string" },
      repeats: { type: "string" },
      concurrency: { type: "string" },
      "per-session-runs": { type: "string" },
      sizes: { type: "string" },
      report: { type: "string" },
      "dispatcher-url": { type: "string" },
    },
  });

  const dispatcherUrl = values["dispatcher-url"] ?? DEFAULT_DISPATCHER_URL;
  const sessionCount = Number.parseInt(values.sessions ?? "10", 10);
  const execRepeats = Number.parseInt(values.repeats ?? "20", 10);
  const concurrencyLevels = parseIntList(values.concurrency, [1, 4, 8, 16]);
  const perSessionRuns = Number.parseInt(values["per-session-runs"] ?? "3", 10);
  const fileSizes = parseIntList(values.sizes, [4 * 1024, 64 * 1024, 1_048_576]);

  if (!values.reuse) {
    if (!(await commandExists("docker"))) {
      throw new Error("docker is required");
    }
    if (!values["skip-build"]) {
      await buildImages({ skipExisting: true });
    }
    await retagImages();
    await composeUp();
  }

  try {
    const readyz = await waitForReadyz(dispatcherUrl);
    const startSnapshot = await runnerSnapshot();

    // Warmup: a single short-lived session flushes the template worker
    // so the first measured create doesn't pay the cold-cache tax.
    const warm = await createSandboxWithBackoff(dispatcherUrl);
    await warm.execute("echo warm").catch(() => {});
    await warm.stop().catch(() => {});

    const sessionCreate = await phaseSessionCreate(dispatcherUrl, sessionCount);

    const executeLatency = await phaseExecuteLatency(
      dispatcherUrl,
      {
        "echo":        "echo hello",
        "pwd":         "pwd",
        "python -c":   "python3 -c 'print(42)'",
        "python math": "python3 -c 'import math; print(math.sqrt(2))'",
      },
      execRepeats,
    );

    const throughput = [];
    for (const concurrency of concurrencyLevels) {
      throughput.push(
        await phaseConcurrentSessions(dispatcherUrl, concurrency, perSessionRuns),
      );
    }

    const fileRoundTrip = await phaseFileRoundTrip(dispatcherUrl, fileSizes);

    const endSnapshot = await runnerSnapshot();
    const metricsText = await runnerPrometheusText();

    const report = {
      suite: "wasmsh-scalable-benchmark",
      recordedAt: new Date().toISOString(),
      dispatcherUrl,
      readyz,
      config: {
        sessionCount,
        execRepeats,
        concurrencyLevels,
        perSessionRuns,
        fileSizes,
      },
      phases: {
        sessionCreate,
        executeLatency,
        throughput,
        fileRoundTrip,
      },
      runnerSnapshotStart: startSnapshot,
      runnerSnapshot: endSnapshot,
      runnerPrometheus: metricsText,
    };

    console.log(renderSummary(report));

    if (values.report) {
      await writeFile(values.report, JSON.stringify(report, null, 2));
      console.log(`report written to ${values.report}`);
    }
  } finally {
    if (!values.reuse && !values.keep) {
      await composeDown();
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
