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

  const rl = readline.createInterface({
    input: process.stdin,
    crlfDelay: Infinity,
  });

  for await (const line of rl) {
    if (!line.trim()) {
      continue;
    }
    let request;
    try {
      request = JSON.parse(line);
      const ALLOWED = new Set(["init", "run", "writeFile", "readFile", "listDir", "installPythonPackages", "close"]);
      const method = request.method;
      if (!ALLOWED.has(method)) {
        throw new Error(`unknown method: ${method}`);
      }
      const result = await host[method](request.params ?? {});
      process.stdout.write(`${JSON.stringify({ id: request.id, ok: true, result })}\n`);
      if (method === "close") {
        rl.close();
        break;
      }
    } catch (error) {
      process.stdout.write(
        `${JSON.stringify({
          id: request?.id ?? null,
          ok: false,
          error: error instanceof Error ? error.message : String(error),
        })}\n`,
      );
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
