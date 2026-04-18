import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  createNodeHostSession,
  RequestClient,
} from "./lib/node-host-session.mjs";
import { encodeBase64 } from "./lib/protocol.mjs";

export const DEFAULT_WORKSPACE_DIR = "/workspace";

const packageDir = path.dirname(fileURLToPath(import.meta.url));

class BrowserWorkerSession extends RequestClient {
  constructor(worker, timeoutMs) {
    let nextId = 1;
    const pending = new Map();

    worker.addEventListener("message", (event) => {
      const response = event.data;
      const entry = pending.get(response.id);
      if (!entry) {
        return;
      }
      pending.delete(response.id);
      if (response.ok) {
        entry.resolve(response.result);
      } else {
        entry.reject(new Error(response.error));
      }
    });

    worker.addEventListener("error", (event) => {
      const error = new Error(`Worker error: ${event.message ?? "unknown"}`);
      for (const entry of pending.values()) {
        entry.reject(error);
      }
      pending.clear();
    });

    super((method, params) => {
      const id = nextId;
      nextId += 1;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        worker.postMessage({ id, method, params });
      });
    }, timeoutMs);

    this._worker = worker;
  }

  async close() {
    try {
      await this._sendRequest("close", {});
    } finally {
      this._worker.terminate();
    }
  }
}

export function resolveAssetPath(...segments) {
  return path.join(packageDir, "assets", ...segments);
}

export function resolveNodeHostPath() {
  return path.join(packageDir, "node-host.mjs");
}

export function resolveBrowserWorkerPath() {
  return new URL("./browser-worker.js", import.meta.url);
}

export async function createNodeSession(options = {}) {
  return createNodeHostSession({
    nodeExecutable: options.nodeExecutable ?? process.execPath,
    hostPath: resolveNodeHostPath(),
    assetDir: options.assetDir ?? resolveAssetPath(),
    timeoutMs: options.timeoutMs,
    initOptions: {
      stepBudget: options.stepBudget ?? 0,
      initialFiles: options.initialFiles ?? [],
      allowedHosts: options.allowedHosts ?? [],
    },
  });
}

export async function createBrowserWorkerSession(options) {
  const worker =
    options.worker ?? new Worker(resolveBrowserWorkerPath(), { name: "wasmsh-pyodide" });
  const session = new BrowserWorkerSession(worker, options.timeoutMs);
  await session._sendRequest("init", {
    assetBaseUrl: options.assetBaseUrl,
    stepBudget: options.stepBudget ?? 0,
    initialFiles: (options.initialFiles ?? []).map((file) => ({
      path: file.path,
      contentBase64: encodeBase64(file.content),
    })),
    allowedHosts: options.allowedHosts ?? [],
  });
  return session;
}
