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
  snapshotBuffer,
  snapshotLength,
  allowedHosts,
  stepBudget,
  initialFiles,
  metrics,
  fetchBroker,
  queueDepth,
  workerEnv,
  brokerBufferOptions,
  workerResourceLimits,
  compiledWasmModule,
}) {
  const restore = metrics.startRestore(queueDepth);
  restore.beginStage("worker_spawn");
  let brokerBuffers = allowedHosts.length > 0 ? createBrokerBuffers(brokerBufferOptions) : null;
  const worker = new Worker(sessionWorkerPath, {
    env: workerEnv,
    resourceLimits: workerResourceLimits,
    workerData: {
      assetDir,
      snapshotBuffer,
      snapshotLength,
      allowedHosts,
      stepBudget,
      initialFiles: normalizeInitialFiles(initialFiles),
      ...(brokerBuffers ?? {}),
      compiledWasmModule,
    },
  });

  const sessionId = randomUUID();
  let nextRequestId = 1;
  const pending = new Map();
  let workerExited = false;
  let workerExitError = null;
  let resolveExit;
  let cleanedUp = false;
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

  function cleanupWorkerState() {
    if (cleanedUp) {
      return;
    }
    cleanedUp = true;
    worker.off("message", onMessage);
    worker.off("error", onError);
    worker.off("exit", onExit);
    brokerBuffers = null;
  }

  const onMessage = async (message) => {
    if (message?.type === "broker-fetch") {
      if (!brokerBuffers) {
        // Worker was started without a broker but tried to use one.  This
        // is a programming error; surface it as a worker failure so we do
        // not leave the guest Atomics.waiting for 30s on a reply that
        // never comes.
        workerExitError = new Error(
          "session worker issued broker-fetch but no broker buffers were provisioned",
        );
        rejectPending(workerExitError);
        await worker.terminate().catch(() => {});
        return;
      }
      try {
        await fetchBroker.handleFetchMessage({
          ...brokerBuffers,
          allowedHosts,
        });
      } catch (error) {
        // handleFetchMessage is already defensive against fetch failures;
        // reaching here means a host-side bug (corrupt shared buffers,
        // JSON parse on the *request*, etc).  Terminate the worker rather
        // than leave the guest blocked indefinitely.
        workerExitError = error instanceof Error ? error : new Error(String(error));
        rejectPending(workerExitError);
        await worker.terminate().catch(() => {});
      }
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
  };
  const onError = (error) => {
    workerExited = true;
    workerExitError = error;
    rejectPending(error);
    cleanupWorkerState();
  };
  const onExit = (code) => {
    workerExited = true;
    resolveExit(code);
    if (!workerExitError && code !== 0) {
      workerExitError = new Error(`session worker exited with code ${code}`);
    } else if (!workerExitError) {
      workerExitError = new Error("session worker has been closed");
    }
    rejectPending(workerExitError);
    cleanupWorkerState();
  };

  worker.on("message", onMessage);
  worker.on("error", onError);
  worker.on("exit", onExit);

  const ready = await new Promise((resolveReady, rejectReady) => {
    const readyTimeoutMs = Number(
      process.env.WASMSH_SESSION_WORKER_READY_TIMEOUT_MS ?? 60_000,
    );
    let settled = false;
    const settle = (fn, value) => {
      if (settled) {
        return;
      }
      settled = true;
      worker.off("message", onReadyMessage);
      worker.off("error", onReadyError);
      worker.off("exit", onReadyExit);
      clearTimeout(timeoutHandle);
      fn(value);
    };
    const onReadyMessage = (message) => {
      if (message?.type === "ready") {
        settle(resolveReady, message);
      } else if (message?.type === "error") {
        settle(rejectReady, new Error(message.error));
      }
    };
    const onReadyError = (error) => {
      settle(rejectReady, error);
    };
    const onReadyExit = (code) => {
      // Regardless of exit code, a worker that exits before emitting
      // `ready` is a restore failure.  Previously only non-zero codes
      // rejected, which let a clean `process.exit(0)` from a top-level
      // module throw (or a snapshot that booted without emitting ready)
      // hang the parent `await` forever.
      settle(rejectReady, new Error(`session worker exited before ready with code ${code}`));
    };
    const timeoutHandle = setTimeout(() => {
      settle(rejectReady, new Error(`session worker ready timed out after ${readyTimeoutMs}ms`));
    }, readyTimeoutMs);
    timeoutHandle.unref?.();
    worker.on("message", onReadyMessage);
    worker.on("error", onReadyError);
    worker.on("exit", onReadyExit);
  }).catch(async (error) => {
    restore.fail();
    try {
      await worker.terminate();
    } catch {
      // terminate() after an already-exited worker throws; ignore
    }
    cleanupWorkerState();
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
