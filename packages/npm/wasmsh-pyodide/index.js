import cp from "node:child_process";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

import { decodeBase64, encodeBase64 } from "./lib/protocol.mjs";

export const DEFAULT_WORKSPACE_DIR = "/workspace";

/** Default request timeout in milliseconds (5 minutes). */
const DEFAULT_TIMEOUT_MS = 5 * 60 * 1000;

const packageDir = path.dirname(fileURLToPath(import.meta.url));

function normalizeInitialFiles(initialFiles = []) {
  return initialFiles.map((file) => ({
    path: file.path,
    contentBase64: encodeBase64(file.content),
  }));
}

class RequestClient {
  constructor(sendRequest, timeoutMs = DEFAULT_TIMEOUT_MS) {
    this._sendRaw = sendRequest;
    this._timeoutMs = timeoutMs;
  }

  _sendRequest(method, params) {
    const promise = this._sendRaw(method, params);
    if (!this._timeoutMs || this._timeoutMs <= 0) {
      return promise;
    }
    let timeoutId;
    const timeoutPromise = new Promise((_, reject) => {
      timeoutId = setTimeout(
        () => reject(new Error(`wasmsh: request '${method}' timed out after ${this._timeoutMs}ms`)),
        this._timeoutMs,
      );
      timeoutId.unref?.();
    });
    return Promise.race([promise, timeoutPromise]).finally(() => {
      clearTimeout(timeoutId);
    });
  }

  async run(command) {
    return this._sendRequest("run", { command });
  }

  async writeFile(pathname, content) {
    return this._sendRequest("writeFile", {
      path: pathname,
      contentBase64: encodeBase64(content),
    });
  }

  async readFile(pathname) {
    const result = await this._sendRequest("readFile", { path: pathname });
    return {
      events: result.events,
      content: decodeBase64(result.contentBase64),
    };
  }

  async listDir(pathname) {
    return this._sendRequest("listDir", { path: pathname });
  }

  async installPythonPackages(requirements, options) {
    return this._sendRequest("installPythonPackages", {
      requirements,
      options: options ?? {},
    });
  }
}

class NodeSession extends RequestClient {
  constructor(child, timeoutMs) {
    let nextId = 1;
    const pending = new Map();
    const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });

    rl.on("line", (line) => {
      if (!line.trim()) {
        return;
      }
      let response;
      try {
        response = JSON.parse(line);
      } catch {
        // Skip non-JSON output (e.g., Emscripten warnings on stdout)
        return;
      }
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

    child.on("error", (error) => {
      for (const entry of pending.values()) {
        entry.reject(error);
      }
      pending.clear();
    });

    child.on("exit", (code) => {
      if (pending.size === 0) {
        return;
      }
      const error = new Error(`wasmsh node host exited with code ${code}`);
      for (const entry of pending.values()) {
        entry.reject(error);
      }
      pending.clear();
    });

    let closed = false;

    super((method, params) => {
      if (closed) {
        return Promise.reject(new Error("wasmsh: session is closed"));
      }
      const id = nextId;
      nextId += 1;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
      });
    }, timeoutMs);

    this._child = child;
    this._rl = rl;
    this._setClosed = () => { closed = true; };
  }

  async close() {
    try {
      await this._sendRaw("close", {});
    } finally {
      this._setClosed();
      this._rl.close();
      this._child.stdin.end();
      if (!this._child.killed) {
        this._child.kill();
      }
    }
  }
}

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
  const child = cp.spawn(
    options.nodeExecutable ?? process.execPath,
    [
      resolveNodeHostPath(),
      "--asset-dir",
      options.assetDir ?? resolveAssetPath(),
    ],
    {
      stdio: ["pipe", "pipe", "inherit"],
    },
  );

  const session = new NodeSession(child, options.timeoutMs);
  await session._sendRequest("init", {
    stepBudget: options.stepBudget ?? 0,
    initialFiles: normalizeInitialFiles(options.initialFiles),
    allowedHosts: options.allowedHosts ?? [],
  });
  return session;
}

export async function createBrowserWorkerSession(options) {
  const worker =
    options.worker ?? new Worker(resolveBrowserWorkerPath(), { name: "wasmsh-pyodide" });
  const session = new BrowserWorkerSession(worker, options.timeoutMs);
  await session._sendRequest("init", {
    assetBaseUrl: options.assetBaseUrl,
    stepBudget: options.stepBudget ?? 0,
    initialFiles: normalizeInitialFiles(options.initialFiles),
    allowedHosts: options.allowedHosts ?? [],
  });
  return session;
}
