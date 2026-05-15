import { dirname, resolve } from "node:path";
import { readFileSync, readdirSync } from "node:fs";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

import { createFullModule } from "./lib/node-module.mjs";
import { installPackages, handlePipCommand } from "./lib/install.mjs";
import {
  buildRunResult,
  encodeBase64,
  extractStream,
  getVersion,
} from "./lib/protocol.mjs";
import { createRuntimeBridge } from "./lib/runtime-bridge.mjs";
import { PTC_HELPER_SOURCE } from "./lib/ptc-helper.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));

function resolveDefaultAssetDir() {
  return resolve(__dirname, "assets");
}

function parseArgs(argv) {
  const options = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--asset-dir" && argv[i + 1]) {
      options.assetDir = argv[i + 1];
      i += 1;
    }
  }
  return options;
}

/** Cache of package names whose wheel files are actually present in assets.
 *  Only packages with a local wheel are considered "bundled" — the lockfile
 *  may list hundreds of packages from the Pyodide CDN that we don't ship.
 *  Set (not Map) because the Node path checks file existence at cache-build
 *  time via readdirSync.  Valid for the lifetime of this process (the asset
 *  directory is immutable once the host subprocess starts). */
let _bundledPackagesCache = null;

function loadBundledPackageNames(assetDir) {
  if (_bundledPackagesCache) return _bundledPackagesCache;
  const lockPath = resolve(assetDir, "pyodide-lock.json");
  try {
    const raw = readFileSync(lockPath, "utf-8");
    let lock;
    try {
      lock = JSON.parse(raw);
    } catch (parseErr) {
      process.stderr.write(`[wasmsh] Failed to parse ${lockPath}: ${parseErr.message}\n`);
      _bundledPackagesCache = new Set();
      return _bundledPackagesCache;
    }
    const localFiles = new Set(readdirSync(assetDir));
    _bundledPackagesCache = new Set(
      Object.entries(lock.packages || {})
        .filter(([, entry]) => entry.file_name && localFiles.has(entry.file_name))
        .map(([name]) => name),
    );
  } catch (err) {
    if (err.code !== "ENOENT") {
      process.stderr.write(`[wasmsh] Failed to load bundled package index from ${lockPath}: ${err.message}\n`);
    }
    _bundledPackagesCache = new Set();
  }
  return _bundledPackagesCache;
}

class WasmshNodeHost {
  constructor(assetDir) {
    this.assetDir = assetDir;
    this.module = null;
    this.runtimeBridge = null;
    this._allowedHosts = [];
    // PTC bridge state. Keyed by host_call id; entries hold the deferred
    // promise that resolves when the host posts a matching host_call_result.
    this._pendingHostCalls = new Map();
    this._ptcHelperLoaded = false;
    this._hostCallSeq = 0;
    this._writeLine = null; // installed by main()
  }

  /** Capabilities the host advertises. Bumped when the wire shape changes. */
  capabilities() {
    return { host_call: "v1" };
  }

  setWriter(writeLine) {
    this._writeLine = writeLine;
  }

  _emit(message) {
    if (!this._writeLine) {
      throw new Error("WasmshNodeHost writer not installed");
    }
    this._writeLine(message);
  }

  handleHostCallResult(message) {
    const pending = this._pendingHostCalls.get(message.id);
    if (!pending) {
      process.stderr.write(
        `[wasmsh] host_call_result for unknown id ${message.id}\n`,
      );
      return;
    }
    this._pendingHostCalls.delete(message.id);
    if (message.ok) {
      pending.resolve(message.value);
    } else {
      const err = new Error(message.message || "host tool error");
      err.name = message.error || "ToolError";
      err.stack = message.stack || err.stack;
      pending.reject(err);
    }
  }

  _toPlainArgs(jsValue) {
    if (jsValue == null) return {};
    // pyodide PyProxy (dict) → plain object.
    if (typeof jsValue.toJs === "function") {
      const converted = jsValue.toJs({ dict_converter: Object.fromEntries });
      // Do NOT destroy: Pyodide still tracks the proxy via its own GC
      // and a manual destroy here races with gc_register_proxies on the
      // awaiting coroutine's resume path. Let Pyodide reclaim it.
      return converted;
    }
    return jsValue;
  }

  _makeHostCallBridge() {
    const self = this;
    return (toolName, argsJsObj) => new Promise((resolve, reject) => {
      const id = `hc_${process.pid}_${++self._hostCallSeq}`;
      self._pendingHostCalls.set(id, { resolve, reject });
      let args;
      try {
        args = self._toPlainArgs(argsJsObj);
      } catch (err) {
        self._pendingHostCalls.delete(id);
        reject(err);
        return;
      }
      self._emit({ type: "host_call", id, tool: String(toolName), args });
    });
  }

  async _ensurePtcHelper() {
    if (this._ptcHelperLoaded) return;
    const pyodide = this.module?._pyodide;
    if (!pyodide) {
      throw new Error("Pyodide API not available — runPtc requires booted runtime");
    }
    pyodide.runPython(PTC_HELPER_SOURCE);
    this._ptcHelperLoaded = true;
  }

  async ensureBooted() {
    if (this.module && this.runtimeBridge) {
      return;
    }
    // Polyfill __dirname/__filename for Deno — pyodide.asm.js needs them
    // to resolve its own location within the asset directory.
    if (typeof globalThis.__dirname === "undefined") {
      globalThis.__dirname = this.assetDir;
      globalThis.__filename = resolve(this.assetDir, "pyodide.asm.js");
    }
    this.module = await createFullModule(this.assetDir);
    this.runtimeBridge = createRuntimeBridge(this.module);
  }

  sendHostCommand(command) {
    if (!this.module || !this.runtimeBridge) {
      throw new Error("runtime not initialized");
    }
    return this.runtimeBridge.sendHostCommand(command);
  }

  async init({ stepBudget = 0, initialFiles = [], allowedHosts = [] } = {}) {
    await this.ensureBooted();
    this._allowedHosts = allowedHosts;
    const events = this.sendHostCommand({
      Init: { step_budget: stepBudget, allowed_hosts: allowedHosts },
    });
    for (const file of initialFiles) {
      this.sendHostCommand({
        WriteFile: {
          path: file.path,
          data: Array.from(Buffer.from(file.contentBase64, "base64")),
        },
      });
    }
    return { events, version: getVersion(events) };
  }

  async runPtc({ code, tools = [] } = {}) {
    if (typeof code !== "string") {
      throw new Error("runPtc requires `code` (string)");
    }
    if (!Array.isArray(tools)) {
      throw new Error("runPtc `tools` must be an array of names");
    }
    await this.ensureBooted();
    const pyodide = this.module?._pyodide;
    if (!pyodide) {
      throw new Error("Pyodide API not available — cannot runPtc");
    }
    await this._ensurePtcHelper();
    // Install bridge + tools namespace into pyodide module globals so the
    // helper picks them up via globals(). We deliberately do NOT call
    // .destroy() on PyProxies obtained via globals.get; Pyodide manages
    // their lifecycle through its own GC, and manual destroy races with
    // gc_register_proxies during/after the awaited coroutine.
    pyodide.globals.set("__wasmsh_host_call", this._makeHostCallBridge());
    pyodide.runPython(
      `_wasmsh_install_tools(${JSON.stringify(tools)})`,
    );
    try {
      const envelopeJson = await pyodide.runPythonAsync(
        `await _wasmsh_run_ptc_block(${JSON.stringify(code)})`,
      );
      let envelope;
      try {
        envelope = JSON.parse(envelopeJson);
      } catch (parseErr) {
        throw new Error(`runPtc envelope parse error: ${parseErr.message}`);
      }
      return { envelope };
    } finally {
      pyodide.globals.delete("__wasmsh_host_call");
      pyodide.globals.delete("tools");
      // Fail any still-pending host calls so a future runPtc doesn't try to
      // resolve into a dead Python coroutine.
      for (const [id, pending] of this._pendingHostCalls) {
        const err = new Error("runPtc completed with unresolved host_call");
        err.name = "PTCAbandonedError";
        pending.reject(err);
        this._pendingHostCalls.delete(id);
      }
    }
  }

  async run({ command }) {
    // Intercept pip commands — PyRun_SimpleString doesn't support
    // top-level await so we route through the JS install path instead.
    const pyodide = this.module?._pyodide;
    if (pyodide) {
      const pipResult = await handlePipCommand(
        command, pyodide,
        (opts) => this.installPythonPackages(opts),
      );
      if (pipResult) return pipResult;
    }
    const events = this.sendHostCommand({ Run: { input: command } });
    return buildRunResult(events);
  }

  async writeFile({ path, contentBase64 }) {
    const events = this.sendHostCommand({
      WriteFile: {
        path,
        data: Array.from(Buffer.from(contentBase64, "base64")),
      },
    });
    return { events };
  }

  async readFile({ path }) {
    const events = this.sendHostCommand({ ReadFile: { path } });
    const content = extractStream(events, "Stdout");
    return { events, contentBase64: encodeBase64(content) };
  }

  async listDir({ path }) {
    const events = this.sendHostCommand({ ListDir: { path } });
    return {
      events,
      output: Buffer.from(extractStream(events, "Stdout")).toString("utf-8"),
    };
  }

  async installPythonPackages({ requirements, options = {} }) {
    await this.ensureBooted();
    const reqs = typeof requirements === "string" ? [requirements] : requirements;
    if (!Array.isArray(reqs)) {
      throw new Error("requirements must be a string or array of strings");
    }

    const pyodide = this.module._pyodide;
    if (!pyodide) {
      throw new Error("Pyodide API not available — cannot install packages");
    }

    const bundled = loadBundledPackageNames(this.assetDir);
    return installPackages(reqs, pyodide, {
      isBundled: (name) => bundled.has(name),
      allowedHosts: this._allowedHosts,
      deps: options.deps,
    });
  }

  async close() {
    if (this.runtimeBridge) {
      this.runtimeBridge.close();
    }
    this.module = null;
    this.runtimeBridge = null;
    return { closed: true };
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const host = new WasmshNodeHost(args.assetDir ?? resolveDefaultAssetDir());

  const writeLine = (obj) => {
    process.stdout.write(`${JSON.stringify(obj)}\n`);
  };
  host.setWriter(writeLine);

  // Advertise capabilities on boot so the host adapter can gate PTC on it.
  writeLine({ type: "ack", capabilities: host.capabilities() });

  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  const ALLOWED = new Set([
    "init",
    "run",
    "runPtc",
    "writeFile",
    "readFile",
    "listDir",
    "installPythonPackages",
    "close",
  ]);

  // Dispatch each request without blocking the readline loop; otherwise a
  // long-running `runPtc` (which itself depends on inbound `host_call_result`
  // lines being read) would deadlock the host on its own stdin.
  const dispatch = (request) => {
    const method = request?.method;
    if (!ALLOWED.has(method)) {
      writeLine({
        id: request?.id ?? null,
        ok: false,
        error: `unknown method: ${method}`,
      });
      return;
    }
    Promise.resolve()
      .then(() => host[method](request.params ?? {}))
      .then(
        (result) => {
          writeLine({ id: request.id, ok: true, result });
          if (method === "close") {
            rl.close();
          }
        },
        (error) => {
          writeLine({
            id: request?.id ?? null,
            ok: false,
            error: error instanceof Error ? error.message : String(error),
          });
        },
      );
  };

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }
    let message;
    try {
      message = JSON.parse(line);
    } catch (parseError) {
      writeLine({
        id: null,
        ok: false,
        error: `invalid JSON on stdin: ${parseError.message}`,
      });
      continue;
    }

    // Out-of-band PTC response. No id/method shape; route directly.
    if (message && message.type === "host_call_result") {
      host.handleHostCallResult(message);
      continue;
    }

    dispatch(message);
    if (message?.method === "close") {
      break;
    }
  }

  rl.close();
}

if (fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exit(1);
  });
}
