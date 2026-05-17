import cp from "node:child_process";
import readline from "node:readline";
import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

import { decodeBase64, encodeBase64 } from "./protocol.mjs";

export const DEFAULT_TIMEOUT_MS = 5 * 60 * 1000;
const defaultCompileCacheDir = resolve(
  process.env.WASMSH_NODE_COMPILE_CACHE_DIR ?? tmpdir(),
  "wasmsh-node-compile-cache",
);

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

  /** Return capabilities the host advertised on boot (subclass-populated). */
  get capabilities() {
    return { ...(this._capabilities ?? {}) };
  }

  /**
   * Run a code block with programmatic tool calling enabled. Optional surface
   * implemented by transports that can demultiplex `host_call` events from
   * the response stream.
   */
  async runPtc(_params) {
    throw new Error(
      "this transport does not support runPtc; " +
        "use NodeSession or a runPtc-capable subclass",
    );
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
    const ptcDispatchers = new Map(); // active per-runPtc onHostCall callbacks
    const capabilities = {};
    const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });
    let exited = false;
    let exitResolve;
    const exitPromise = new Promise((resolve) => {
      exitResolve = resolve;
    });

    const writeLine = (msg) => {
      child.stdin.write(`${JSON.stringify(msg)}\n`);
    };

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
      // Out-of-band envelopes from the host. None carry a JSON-RPC id.
      if (response && typeof response === "object" && response.type) {
        if (response.type === "ack" && response.capabilities) {
          Object.assign(capabilities, response.capabilities);
          return;
        }
        if (response.type === "host_call") {
          // Route to every active dispatcher; only the request whose code
          // emitted the call has a matching dispatcher entry. Since runPtc
          // is serialised per session (host runs one async eval at a time),
          // there is typically only one entry. Each dispatcher itself
          // writes back the host_call_result.
          for (const dispatch of ptcDispatchers.values()) {
            dispatch(response);
          }
          return;
        }
        // host_call_result is client→host; ignore if the host ever echoes one.
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
        writeLine({ id, method, params });
      });
    }, timeoutMs);
    this._capabilities = capabilities;
    this._writeLine = writeLine;
    this._ptcDispatchers = ptcDispatchers;

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

  async runPtc({ code, tools = [], onHostCall }) {
    if (typeof code !== "string") {
      throw new TypeError("runPtc requires `code` (string)");
    }
    if (!Array.isArray(tools)) {
      throw new TypeError("runPtc `tools` must be an array of names");
    }
    if (typeof onHostCall !== "function") {
      throw new TypeError("runPtc requires `onHostCall` (async function)");
    }
    if (!this._capabilities.host_call) {
      throw new Error(
        "wasmsh host did not advertise host_call capability; " +
          "PTC is not supported by this runtime build",
      );
    }
    const dispatch = (hostCall) => {
      Promise.resolve()
        .then(() => onHostCall(hostCall))
        .catch((error) => ({
          ok: false,
          error: error?.name ?? "Error",
          message: error?.message ?? String(error),
        }))
        .then((envelope) => {
          this._writeLine({
            type: "host_call_result",
            id: hostCall.id,
            ...envelope,
          });
        });
    };
    // Register before sending so an early `host_call` is routed correctly.
    const key = Symbol("ptc");
    this._ptcDispatchers.set(key, dispatch);
    try {
      const result = await this._sendRequest("runPtc", {
        code,
        tools: [...tools],
      });
      const envelope = result?.envelope;
      if (!envelope || typeof envelope !== "object") {
        throw new Error(
          "runPtc returned no envelope; host adapter is out of sync",
        );
      }
      return envelope;
    } finally {
      this._ptcDispatchers.delete(key);
    }
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
  mkdirSync(defaultCompileCacheDir, { recursive: true });
  const child = cp.spawn(
    options.nodeExecutable ?? process.execPath,
    [options.hostPath, "--asset-dir", options.assetDir],
    {
      env: {
        ...process.env,
        NODE_COMPILE_CACHE: process.env.NODE_COMPILE_CACHE ?? defaultCompileCacheDir,
      },
      stdio: ["pipe", "pipe", "inherit"],
    },
  );

  const session = new NodeSession(child, options.timeoutMs);
  if (options.autoInit !== false) {
    await session.init(options.initOptions);
  }
  return session;
}
