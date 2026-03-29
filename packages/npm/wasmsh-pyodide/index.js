import cp from "node:child_process";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

export const DEFAULT_WORKSPACE_DIR = "/workspace";

const packageDir = path.dirname(fileURLToPath(import.meta.url));

function encodeBase64(bytes) {
  return Buffer.from(bytes).toString("base64");
}

function decodeBase64(text) {
  return new Uint8Array(Buffer.from(text, "base64"));
}

function normalizeInitialFiles(initialFiles = []) {
  return initialFiles.map((file) => ({
    path: file.path,
    contentBase64: encodeBase64(file.content),
  }));
}

class RequestClient {
  constructor(sendRequest) {
    this._sendRequest = sendRequest;
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
}

class NodeSession extends RequestClient {
  constructor(child) {
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

    super((method, params) => {
      const id = nextId;
      nextId += 1;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
      });
    });

    this._child = child;
    this._rl = rl;
  }

  async close() {
    try {
      await this._sendRequest("close", {});
    } finally {
      this._rl.close();
      this._child.stdin.end();
      if (!this._child.killed) {
        this._child.kill();
      }
    }
  }
}

class BrowserWorkerSession extends RequestClient {
  constructor(worker) {
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
    });

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

  const session = new NodeSession(child);
  await session._sendRequest("init", {
    stepBudget: options.stepBudget ?? 0,
    initialFiles: normalizeInitialFiles(options.initialFiles),
  });
  return session;
}

export async function createBrowserWorkerSession(options) {
  const worker =
    options.worker ?? new Worker(resolveBrowserWorkerPath(), { name: "wasmsh-pyodide" });
  const session = new BrowserWorkerSession(worker);
  await session._sendRequest("init", {
    assetBaseUrl: options.assetBaseUrl,
    stepBudget: options.stepBudget ?? 0,
    initialFiles: normalizeInitialFiles(options.initialFiles),
  });
  return session;
}
