import { Worker } from "node:worker_threads";
import { randomUUID } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { createBrokerBuffers } from "./fetch-broker.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const sessionWorkerPath = resolve(__dirname, "./session-worker.mjs");

export const MAX_INLINE_INITIAL_BYTES = 64 * 1024;

function normalizeInitialFiles(initialFiles = []) {
  const files = initialFiles.map((file) => ({
    path: file.path,
    content: Uint8Array.from(file.content),
  }));
  const totalBytes = files.reduce((sum, file) => sum + file.content.byteLength, 0);
  if (totalBytes > MAX_INLINE_INITIAL_BYTES) {
    throw new Error("initialFiles payload is too large for the inline create path");
  }
  return files;
}

export async function restoreSessionWorker({
  assetDir,
  snapshotBytes,
  allowedHosts,
  stepBudget,
  initialFiles,
  metrics,
  fetchBroker,
  queueDepth,
}) {
  const restore = metrics.startRestore(queueDepth);
  restore.beginStage("worker_spawn");
  const brokerBuffers = createBrokerBuffers();
  const worker = new Worker(sessionWorkerPath, {
    workerData: {
      assetDir,
      snapshotBytes,
      allowedHosts,
      stepBudget,
      initialFiles: normalizeInitialFiles(initialFiles),
      ...brokerBuffers,
    },
  });

  const sessionId = randomUUID();
  let nextRequestId = 1;
  const pending = new Map();
  let workerExited = false;
  let workerExitError = null;
  let resolveExit;
  const exitPromise = new Promise((resolve) => {
    resolveExit = resolve;
  });

  function rejectPending(error) {
    for (const entry of pending.values()) {
      entry.reject(error);
    }
    pending.clear();
  }

  function ensureWorkerActive() {
    if (workerExited) {
      throw workerExitError ?? new Error("session worker is no longer available");
    }
  }

  worker.on("message", async (message) => {
    if (message?.type === "broker-fetch") {
      await fetchBroker.handleFetchMessage({
        ...brokerBuffers,
        allowedHosts,
      });
      return;
    }
    if (message?.type === "response") {
      const entry = pending.get(message.id);
      if (!entry) {
        return;
      }
      pending.delete(message.id);
      if (message.ok) {
        entry.resolve(message.result);
      } else {
        entry.reject(new Error(message.error));
      }
    }
  });
  worker.on("error", (error) => {
    workerExited = true;
    workerExitError = error;
    rejectPending(error);
  });
  worker.on("exit", (code) => {
    workerExited = true;
    resolveExit(code);
    if (!workerExitError && code !== 0) {
      workerExitError = new Error(`session worker exited with code ${code}`);
    } else if (!workerExitError) {
      workerExitError = new Error("session worker has been closed");
    }
    rejectPending(workerExitError);
  });

  const ready = await new Promise((resolveReady, rejectReady) => {
    const onMessage = (message) => {
      if (message?.type === "ready") {
        worker.off("message", onMessage);
        resolveReady(message);
      } else if (message?.type === "error") {
        worker.off("message", onMessage);
        rejectReady(new Error(message.error));
      }
    };
    worker.on("message", onMessage);
    worker.on("error", rejectReady);
    worker.on("exit", (code) => {
      if (code !== 0) {
        rejectReady(new Error(`session worker exited with code ${code}`));
      }
    });
  }).catch((error) => {
    restore.fail();
    throw error;
  });

  restore.endStage("worker_spawn");
  restore.beginStage("sandbox_restore");
  const initResult = await sendRequest("init", {});
  restore.endStage("sandbox_restore");
  const restoreResult = restore.finish();

  function sendRequest(method, params) {
    ensureWorkerActive();
    const id = nextRequestId;
    nextRequestId += 1;
    return new Promise((resolveResult, rejectResult) => {
      ensureWorkerActive();
      pending.set(id, {
        resolve: resolveResult,
        reject: rejectResult,
      });
      worker.postMessage({
        type: "request",
        id,
        method,
        params,
      });
    });
  }

  return {
    id: sessionId,
    worker,
    workerId: ready.workerId ?? initResult.workerId,
    restoreMetrics: restoreResult,
    sendRequest,
    waitForExit() {
      return exitPromise;
    },
    terminate() {
      if (workerExited) {
        return Promise.resolve(0);
      }
      return worker.terminate();
    },
  };
}
