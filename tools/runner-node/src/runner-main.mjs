import { Worker } from "node:worker_threads";
import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createFetchBroker } from "./fetch-broker.mjs";
import { createRunnerMetrics } from "./metrics.mjs";
import { restoreSessionWorker } from "./restore-engine.mjs";
import { createSessionRegistry } from "./session-registry.mjs";
import { applyCompileCacheEnv } from "./compile-cache.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const templateWorkerPath = resolve(__dirname, "./template-worker.mjs");

function normalizeContent(content) {
  if (typeof content === "string") {
    return new TextEncoder().encode(content);
  }
  return Uint8Array.from(content);
}

function selftestPassed(selftest = {}) {
  const echoExit = selftest.echo?.find?.((event) => "Exit" in event)?.Exit;
  const pythonExit = selftest.python?.find?.((event) => "Exit" in event)?.Exit;
  return echoExit === 0 && pythonExit === 0;
}

function shareSnapshotBytes(snapshotBytes) {
  const sharedBuffer = new SharedArrayBuffer(snapshotBytes.byteLength);
  new Uint8Array(sharedBuffer).set(snapshotBytes);
  return {
    buffer: sharedBuffer,
    byteLength: snapshotBytes.byteLength,
  };
}

async function loadCompiledWasmModule(assetDir) {
  const wasmBytes = await readFile(resolve(assetDir, "pyodide.asm.wasm"));
  return WebAssembly.compile(wasmBytes);
}

async function warmScratchRestore({
  assetDir,
  snapshotBuffer,
  snapshotLength,
  fetchBroker,
  workerEnv,
  restoreSessionWorkerFn,
  brokerBufferOptions,
  workerResourceLimits,
  compiledWasmModule,
}) {
  const warmMetrics = createRunnerMetrics();
  const warmed = await restoreSessionWorkerFn({
    assetDir,
    snapshotBuffer,
    snapshotLength,
    allowedHosts: [],
    stepBudget: 0,
    initialFiles: [],
    metrics: warmMetrics,
    fetchBroker,
    queueDepth: 0,
    workerEnv,
    brokerBufferOptions,
    workerResourceLimits,
    compiledWasmModule,
  });
  try {
    await warmed.sendRequest("close", {});
    await warmed.waitForExit();
  } finally {
    await warmed.terminate();
  }
}

export async function createRunner(options = {}) {
  const assetDir = options.assetDir ?? resolve(process.cwd(), "packages/npm/wasmsh-pyodide/assets");
  const runnerId = options.runnerId ?? process.env.WASMSH_RUNNER_ID ?? randomUUID();
  const restoreSlots = Number(options.restoreSlots ?? process.env.WASMSH_RESTORE_SLOTS ?? 4);
  const startupWarmRestores = Number(
    options.startupWarmRestores ?? process.env.WASMSH_STARTUP_WARM_RESTORES ?? 2,
  );
  const brokerBufferOptions = {
    requestBytes: Number(
      options.fetchBrokerRequestBytes
      ?? process.env.WASMSH_FETCH_BROKER_REQUEST_BYTES
      ?? 64 * 1024,
    ),
    responseBytes: Number(
      options.fetchBrokerResponseBytes
      ?? process.env.WASMSH_FETCH_BROKER_RESPONSE_BYTES
      ?? 1024 * 1024,
    ),
  };
  const workerResourceLimits = {
    maxOldGenerationSizeMb: Number(
      options.workerMaxOldGenerationSizeMb
      ?? process.env.WASMSH_WORKER_MAX_OLD_GENERATION_MB
      ?? 48,
    ),
    maxYoungGenerationSizeMb: Number(
      options.workerMaxYoungGenerationSizeMb
      ?? process.env.WASMSH_WORKER_MAX_YOUNG_GENERATION_MB
      ?? 8,
    ),
    stackSizeMb: Number(
      options.workerStackSizeMb
      ?? process.env.WASMSH_WORKER_STACK_MB
      ?? 1,
    ),
    codeRangeSizeMb: Number(
      options.workerCodeRangeSizeMb
      ?? process.env.WASMSH_WORKER_CODE_RANGE_MB
      ?? 8,
    ),
  };
  const restoreSessionWorkerFn = options.restoreSessionWorker ?? restoreSessionWorker;
  const metrics = createRunnerMetrics();
  const registry = createSessionRegistry();
  const fetchBroker = createFetchBroker({
    fetchImpl: options.fetchImpl ?? fetch,
    metrics,
  });
  const compiledWasmModule = await loadCompiledWasmModule(assetDir);

  let templateWorker = new Worker(templateWorkerPath, {
    env: applyCompileCacheEnv(),
    workerData: {
      assetDir,
      compiledWasmModule,
    },
  });
  const templateInfo = await new Promise((resolveReady, rejectReady) => {
    templateWorker.on("message", (message) => {
      if (message?.type === "ready") {
        resolveReady(message);
      } else if (message?.type === "error") {
        rejectReady(new Error(message.error));
      }
    });
    templateWorker.on("error", rejectReady);
    templateWorker.on("exit", (code) => {
      if (code !== 0) {
        rejectReady(new Error(`template worker exited with code ${code}`));
      }
    });
  });
  await templateWorker.terminate();
  templateWorker = null;

  const snapshotBytes = Uint8Array.from(templateInfo.snapshotBytes);
  const sharedSnapshot = shareSnapshotBytes(snapshotBytes);
  const workerEnv = applyCompileCacheEnv();
  for (let index = 0; index < startupWarmRestores; index += 1) {
    await warmScratchRestore({
      assetDir,
      snapshotBuffer: sharedSnapshot.buffer,
      snapshotLength: sharedSnapshot.byteLength,
      fetchBroker,
      workerEnv,
      restoreSessionWorkerFn,
      brokerBufferOptions,
      workerResourceLimits,
      compiledWasmModule,
    });
  }
  let pendingCreates = 0;
  let inflightCreateRestores = 0;
  const restoreWaiters = [];

  async function acquireRestoreSlot() {
    if (inflightCreateRestores < restoreSlots) {
      inflightCreateRestores += 1;
      return;
    }
    await new Promise((resolve) => {
      restoreWaiters.push(resolve);
    });
    inflightCreateRestores += 1;
  }

  function releaseRestoreSlot() {
    inflightCreateRestores = Math.max(0, inflightCreateRestores - 1);
    const waiter = restoreWaiters.shift();
    if (waiter) {
      waiter();
    }
  }

  async function createSession({
    sessionId,
    allowedHosts = [],
    initialFiles = [],
    stepBudget = 0,
  } = {}) {
    pendingCreates += 1;
    try {
      await acquireRestoreSlot();
      let restored;
      try {
        restored = await restoreSessionWorkerFn({
          assetDir,
          snapshotBuffer: sharedSnapshot.buffer,
          snapshotLength: sharedSnapshot.byteLength,
          allowedHosts,
          stepBudget,
          initialFiles: initialFiles.map((file) => ({
            path: file.path,
            content: normalizeContent(file.content),
          })),
          metrics,
          fetchBroker,
          queueDepth: Math.max(0, pendingCreates - restoreSlots),
          workerEnv,
          brokerBufferOptions,
          workerResourceLimits,
          compiledWasmModule,
        });
      } finally {
        releaseRestoreSlot();
      }
      metrics.sessionOpened();

      const session = {
        id: sessionId ?? restored.id ?? randomUUID(),
        workerId: restored.workerId,
        restoreMetrics: restored.restoreMetrics,
        closed: false,
        init() {
          return Promise.resolve({
            sessionId: this.id,
            workerId: this.workerId,
          });
        },
        async run(command) {
          return restored.sendRequest("run", { command });
        },
        async writeFile(path, content) {
          return restored.sendRequest("writeFile", {
            path,
            content: normalizeContent(content),
          });
        },
        async readFile(path) {
          return restored.sendRequest("readFile", { path });
        },
        async listDir(path) {
          return restored.sendRequest("listDir", { path });
        },
        async close() {
          if (this.closed) {
            return { closed: true };
          }
          this.closed = true;
          try {
            await restored.sendRequest("close", {});
            await restored.waitForExit();
            return { closed: true };
          } catch (error) {
            if (String(error?.message ?? error).includes("has been closed")) {
              await restored.waitForExit();
              return { closed: true };
            }
            await restored.terminate();
            throw error;
          } finally {
            registry.delete(this.id);
            metrics.sessionClosed();
          }
        },
      };

      registry.add(session);
      return session;
    } finally {
      pendingCreates = Math.max(0, pendingCreates - 1);
    }
  }

  return {
    async createSession(params) {
      return createSession(params);
    },
    listSessions() {
      return registry.list();
    },
    getSession(sessionId) {
      return registry.get(sessionId);
    },
    getTemplateInfo() {
      return {
        workerId: templateInfo.workerId,
        manifest: templateInfo.manifest,
        selftest: templateInfo.selftest,
        ready: selftestPassed(templateInfo.selftest),
      };
    },
    readiness() {
      return {
        ready: selftestPassed(templateInfo.selftest),
        templateWorkerId: templateInfo.workerId,
        snapshotDigest: templateInfo.manifest.snapshot_digest,
      };
    },
    runnerSnapshot() {
      const snapshot = metrics.snapshot();
      return {
        runner_id: runnerId,
        restore_slots: restoreSlots,
        inflight_restores: snapshot.wasmsh_inflight_restores,
        restore_queue_depth: snapshot.wasmsh_restore_queue_depth,
        restore_p95_ms: snapshot.wasmsh_session_restore_duration_ms.p95,
        active_sessions: snapshot.wasmsh_active_sessions,
        draining: false,
        healthy: selftestPassed(templateInfo.selftest),
      };
    },
    metrics: {
      snapshot() {
        return metrics.snapshot();
      },
    },
    async close() {
      for (const session of registry.values()) {
        await session.close();
      }
      if (templateWorker) {
        templateWorker.postMessage({ type: "close" });
        await templateWorker.terminate();
      }
    },
  };
}
