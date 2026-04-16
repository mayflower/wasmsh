import { Worker } from "node:worker_threads";
import { randomUUID } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createFetchBroker } from "./fetch-broker.mjs";
import { createRunnerMetrics } from "./metrics.mjs";
import { restoreSessionWorker } from "./restore-engine.mjs";
import { createSessionRegistry } from "./session-registry.mjs";

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

export async function createRunner(options = {}) {
  const assetDir = options.assetDir ?? resolve(process.cwd(), "packages/npm/wasmsh-pyodide/assets");
  const metrics = createRunnerMetrics();
  const registry = createSessionRegistry();
  const fetchBroker = createFetchBroker({
    fetchImpl: options.fetchImpl ?? fetch,
    metrics,
  });

  const templateWorker = new Worker(templateWorkerPath, {
    workerData: { assetDir },
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

  const snapshotBytes = Uint8Array.from(templateInfo.snapshotBytes);
  let pendingCreates = 0;

  async function createSession({
    allowedHosts = [],
    initialFiles = [],
    stepBudget = 0,
  } = {}) {
    pendingCreates += 1;
    try {
      const restored = await restoreSessionWorker({
        assetDir,
        snapshotBytes,
        allowedHosts,
        stepBudget,
        initialFiles: initialFiles.map((file) => ({
          path: file.path,
          content: normalizeContent(file.content),
        })),
        metrics,
        fetchBroker,
        queueDepth: pendingCreates,
      });
      metrics.sessionOpened();

      const session = {
        id: restored.id ?? randomUUID(),
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
    metrics: {
      snapshot() {
        return metrics.snapshot();
      },
    },
    async close() {
      for (const session of registry.values()) {
        await session.close();
      }
      templateWorker.postMessage({ type: "close" });
      await templateWorker.terminate();
    },
  };
}
