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

class WasmshNodeHost {
  constructor(assetDir) {
    this.assetDir = assetDir;
    this.module = null;
    this.runtimeHandle = null;
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
      const ALLOWED = new Set(["init", "run", "writeFile", "readFile", "listDir", "close"]);
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
