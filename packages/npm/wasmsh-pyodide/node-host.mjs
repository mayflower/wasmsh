import { dirname, resolve } from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

import { createFullModule } from "./lib/node-module.mjs";
import {
  buildRunResult,
  encodeBase64,
  extractStream,
  getVersion,
} from "./lib/protocol.mjs";

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

/**
 * Check if a URL's host is in the allowlist.
 * Mirrors the Rust HostAllowlist semantics: exact host, wildcard subdomain
 * (*.example.com), host:port.  Empty list denies all.
 */
function isHostAllowed(url, allowedHosts) {
  if (!allowedHosts || allowedHosts.length === 0) {
    return false;
  }
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  const host = parsed.hostname.toLowerCase();
  const port = parsed.port ? Number(parsed.port) : null;

  for (const pattern of allowedHosts) {
    const colonIdx = pattern.lastIndexOf(":");
    let patHost, patPort;
    if (colonIdx > 0 && /^\d+$/.test(pattern.slice(colonIdx + 1))) {
      patHost = pattern.slice(0, colonIdx).toLowerCase();
      patPort = Number(pattern.slice(colonIdx + 1));
    } else {
      patHost = pattern.toLowerCase();
      patPort = null;
    }

    if (patPort !== null && port !== patPort) continue;

    if (patHost.startsWith("*.")) {
      const suffix = patHost.slice(2);
      if (host === suffix || host.endsWith(`.${suffix}`)) return true;
    } else {
      if (host === patHost) return true;
    }
  }
  return false;
}

class WasmshNodeHost {
  constructor(assetDir) {
    this.assetDir = assetDir;
    this.module = null;
    this.runtimeHandle = null;
    this._allowedHosts = [];
  }

  async ensureBooted() {
    if (this.module && this.runtimeHandle !== null) {
      return;
    }
    this.module = await createFullModule(this.assetDir);
    this.runtimeHandle = this.module.ccall("wasmsh_runtime_new", "number", [], []);
  }

  sendHostCommand(command) {
    if (!this.module || this.runtimeHandle === null) {
      throw new Error("runtime not initialized");
    }
    const json = JSON.stringify(command);
    const jsonPtr = this.module.stringToNewUTF8(json);
    const resultPtr = this.module.ccall(
      "wasmsh_runtime_handle_json",
      "number",
      ["number", "number"],
      [this.runtimeHandle, jsonPtr],
    );
    this.module._free(jsonPtr);
    const resultStr = this.module.UTF8ToString(resultPtr);
    this.module.ccall("wasmsh_runtime_free_string", null, ["number"], [resultPtr]);
    return JSON.parse(resultStr);
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
    const decoder = new TextDecoder();
    return {
      events,
      output: decoder.decode(extractStream(events, "Stdout")),
    };
  }

  async installPythonPackages({ requirements, options = {} }) {
    await this.ensureBooted();
    const reqs = typeof requirements === "string" ? [requirements] : requirements;
    if (!Array.isArray(reqs)) {
      throw new Error("requirements must be a string or array of strings");
    }

    const installed = [];
    for (const req of reqs) {
      // Security: reject file: URIs to prevent host filesystem escape
      if (/^file:/i.test(req)) {
        throw new Error(`file: URIs are not supported for security: ${req}`);
      }

      if (req.startsWith("emfs:")) {
        // Local wheel install from the in-process Emscripten filesystem
        const wheelPath = req.slice(5);
        const result = await this.run({
          command: `python3 << 'WASMSH_PIP_EOF'
import zipfile, os, sys
sp = '/lib/python3.13/site-packages'
os.makedirs(sp, exist_ok=True)
if sp not in sys.path:
    sys.path.insert(0, sp)
whl = '${wheelPath.replace(/'/g, "'\\''")}'
if not os.path.isfile(whl):
    print('ERR:wheel not found: ' + whl, file=sys.stderr)
    sys.exit(1)
with zipfile.ZipFile(whl) as zf:
    zf.extractall(sp)
print('OK')
WASMSH_PIP_EOF`,
        });
        if (result.exitCode !== 0) {
          throw new Error(
            `Failed to install ${req}: ${result.stderr || result.output}`,
          );
        }
        installed.push({ requirement: req });
      } else if (/^https?:\/\//i.test(req)) {
        // Validate against allowedHosts before attempting download
        if (!isHostAllowed(req, this._allowedHosts)) {
          throw new Error(
            `Host not allowed for package install: ${req}. ` +
            "Configure allowedHosts when creating the session.",
          );
        }
        // TODO: download wheel and install (requires micropip or manual fetch)
        throw new Error(
          `HTTP(S) URL installs are not yet implemented: ${req}. Use emfs: URLs for local wheel files.`,
        );
      } else {
        // Package name — needs micropip + network + allowedHosts
        if (this._allowedHosts.length === 0) {
          throw new Error(
            `Package name installs require network access: ${req}. ` +
            "Configure allowedHosts (e.g., ['pypi.org', 'files.pythonhosted.org']) when creating the session.",
          );
        }
        // TODO: use micropip to resolve and install (requires loadPyodide refactor)
        throw new Error(
          `Package name installs are not yet implemented: ${req}. Use emfs: URLs for local wheel files.`,
        );
      }
    }
    return { installed, requirements: reqs };
  }

  async close() {
    if (this.module && this.runtimeHandle !== null) {
      this.module.ccall("wasmsh_runtime_free", null, ["number"], [this.runtimeHandle]);
    }
    this.module = null;
    this.runtimeHandle = null;
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
