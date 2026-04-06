import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

// Polyfill CJS globals for Deno — Emscripten's pyodide.asm.js expects them.
if (typeof globalThis.require === "undefined") {
  globalThis.require = createRequire(import.meta.url);
}

import { isHostAllowed } from "./lib/allowlist.mjs";
import { createFullModule } from "./lib/node-module.mjs";
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

function pipResult(stdout, stderr, exitCode) {
  return { events: [], stdout, stderr, output: stdout + stderr, exitCode };
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
    const pipResult = await this._handlePipCommand(command);
    if (pipResult) return pipResult;
    const events = this.sendHostCommand({ Run: { input: command } });
    return buildRunResult(events);
  }

  async _handlePipCommand(command) {
    const PIP_PREFIX_RE = /^\s*(?:pip3?|python3?\s+-m\s+pip)(?:\s+|$)/;
    if (!PIP_PREFIX_RE.test(command)) return null;

    const installMatch = command.match(
      /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+install\s+(.+)$/,
    );
    if (installMatch) {
      const packages = installMatch[1]
        .split(/\s+/)
        .filter((a) => a && !a.startsWith("-"));
      if (packages.length === 0) {
        return pipResult("", "Usage: pip install <package> [package ...]\n", 1);
      }
      try {
        await this.installPythonPackages({ requirements: packages });
        const msg = packages.map((p) => `Successfully installed ${p}`).join("\n") + "\n";
        return pipResult(msg, "", 0);
      } catch (err) {
        return pipResult("", `ERROR: ${err.message}\n`, 1);
      }
    }

    const pyodide = this.module?._pyodide;
    if (!pyodide) {
      return pipResult("", "ERROR: Pyodide not initialized\n", 1);
    }

    const uninstallMatch = command.match(
      /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+uninstall\s+(.+)$/,
    );
    if (uninstallMatch) {
      const packages = uninstallMatch[1]
        .split(/\s+/)
        .filter((a) => a && !a.startsWith("-"));
      if (packages.length === 0) {
        return pipResult("", "Usage: pip uninstall <package> [package ...]\n", 1);
      }
      try {
        const micropip = pyodide.pyimport("micropip");
        micropip.uninstall(packages);
        const msg = packages.map((p) => `Successfully uninstalled ${p}`).join("\n") + "\n";
        return pipResult(msg, "", 0);
      } catch (err) {
        return pipResult("", `ERROR: ${err.message}\n`, 1);
      }
    }

    if (/^\s*(?:pip3?|python3?\s+-m\s+pip)\s+list\b/.test(command)) {
      try {
        const micropip = pyodide.pyimport("micropip");
        const pkgDict = micropip.list();
        const entries = [];
        for (const name of pkgDict.keys()) {
          const pkg = pkgDict.get(name);
          entries.push({ name, version: pkg.version });
        }
        pkgDict.destroy();
        entries.sort((a, b) => a.name.localeCompare(b.name));
        const nameW = Math.max(7, ...entries.map((e) => e.name.length));
        const verW = Math.max(7, ...entries.map((e) => e.version.length));
        let out = `${"Package".padEnd(nameW)} ${"Version".padEnd(verW)}\n`;
        out += `${"-".repeat(nameW)} ${"-".repeat(verW)}\n`;
        for (const e of entries) {
          out += `${e.name.padEnd(nameW)} ${e.version.padEnd(verW)}\n`;
        }
        return pipResult(out, "", 0);
      } catch (err) {
        return pipResult("", `ERROR: ${err.message}\n`, 1);
      }
    }

    if (/^\s*(?:pip3?|python3?\s+-m\s+pip)\s+freeze\b/.test(command)) {
      try {
        const micropip = pyodide.pyimport("micropip");
        const frozen = micropip.freeze();
        return pipResult(frozen + "\n", "", 0);
      } catch (err) {
        return pipResult("", `ERROR: ${err.message}\n`, 1);
      }
    }

    // Bare pip or unsupported subcommand — show usage help
    const msg =
      "Usage: pip <command> [options]\n\n" +
      "Commands:\n" +
      "  install     Install packages\n" +
      "  uninstall   Uninstall packages\n" +
      "  list        List installed packages\n" +
      "  freeze      Output installed packages in lockfile format\n";
    return pipResult(msg, "", 0);
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
    const micropip = pyodide.pyimport("micropip");

    const installed = [];
    for (const req of reqs) {
      if (/^file:/i.test(req)) {
        throw new Error(`file: URIs are not supported for security: ${req}`);
      }
      if (/^https?:\/\//i.test(req) && !isHostAllowed(req, this._allowedHosts)) {
        throw new Error(
          `Host not allowed for package install: ${req}. ` +
          "Configure allowedHosts when creating the session.",
        );
      }
      if (!req.startsWith("emfs:") && !/^https?:\/\//i.test(req) && this._allowedHosts.length === 0) {
        throw new Error(
          `Package name installs require network access: ${req}. ` +
          "Configure allowedHosts (e.g., ['cdn.jsdelivr.net', 'pypi.org', 'files.pythonhosted.org']) when creating the session.",
        );
      }

      await micropip.install(req, { deps: options.deps !== false });
      installed.push({ requirement: req });
    }
    return { installed, requirements: reqs };
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
}

if (fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exit(1);
  });
}
