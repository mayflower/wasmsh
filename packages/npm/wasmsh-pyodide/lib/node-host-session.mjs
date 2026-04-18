import cp from "node:child_process";
import readline from "node:readline";

import { decodeBase64, encodeBase64 } from "./protocol.mjs";

export const DEFAULT_TIMEOUT_MS = 5 * 60 * 1000;

function normalizeInitialFiles(initialFiles = []) {
  return initialFiles.map((file) => ({
    path: file.path,
    contentBase64: encodeBase64(file.content),
  }));
}

export class RequestClient {
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

  async init(options = {}) {
    return this._sendRequest("init", {
      stepBudget: options.stepBudget ?? 0,
      initialFiles: normalizeInitialFiles(options.initialFiles),
      allowedHosts: options.allowedHosts ?? [],
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

export class NodeSession extends RequestClient {
  constructor(child, timeoutMs) {
    let nextId = 1;
    const pending = new Map();
    const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });
    let exited = false;
    let exitResolve;
    const exitPromise = new Promise((resolve) => {
      exitResolve = resolve;
    });

    rl.on("line", (line) => {
      if (!line.trim()) {
        return;
      }
      let response;
      try {
        response = JSON.parse(line);
      } catch {
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
      exited = true;
      exitResolve(code);
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
    let closePromise = null;

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
    this._exitPromise = exitPromise;
    this._hasExited = () => exited;
    this._setClosed = () => {
      closed = true;
    };
    this._setClosePromise = (promise) => {
      closePromise = promise;
    };
    this._getClosePromise = () => closePromise;
  }

  async close() {
    const existing = this._getClosePromise();
    if (existing) {
      return existing;
    }

    const closePromise = (async () => {
      try {
        await this._sendRaw("close", {});
      } finally {
        this._setClosed();
        this._rl.close();
        this._child.stdin.end();

        if (!this._hasExited() && !this._child.killed) {
          this._child.kill();
        }

        const waitForExit = (timeoutMs) =>
          Promise.race([
            this._exitPromise,
            new Promise((resolve) => {
              const timeoutId = setTimeout(resolve, timeoutMs);
              timeoutId.unref?.();
            }),
          ]);

        await waitForExit(1_000);

        if (!this._hasExited()) {
          this._child.kill("SIGKILL");
          await waitForExit(1_000);
        }
      }
    })();

    this._setClosePromise(closePromise);
    return closePromise;
  }
}

export async function createNodeHostSession(options = {}) {
  const child = cp.spawn(
    options.nodeExecutable ?? process.execPath,
    [options.hostPath, "--asset-dir", options.assetDir],
    {
      stdio: ["pipe", "pipe", "inherit"],
    },
  );

  const session = new NodeSession(child, options.timeoutMs);
  if (options.autoInit !== false) {
    await session.init(options.initOptions);
  }
  return session;
}
