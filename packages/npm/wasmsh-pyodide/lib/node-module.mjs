import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { createRuntimeBridge } from "./runtime-bridge.mjs";

// Deno's Node compat layer exposes `process` (so Emscripten detects
// ENVIRONMENT_IS_NODE) but does not provide CJS globals in ESM context.
// pyodide.asm.js uses `require("fs")` in its Node path.
if (typeof globalThis.require === "undefined") {
  globalThis.require = createRequire(import.meta.url);
}

const fetchHelperPath = resolve(
  new URL(".", import.meta.url).pathname,
  "fetch-helper.mjs",
);

/**
 * Synchronous HTTP fetch for Node.js — spawns a subprocess that runs
 * fetch-helper.mjs. Request data is passed via stdin (no argv size limits).
 * Returns a JSON object with {status, headers, body_base64}.
 */
const IS_DENO = typeof globalThis.Deno !== "undefined";

export function syncHttpFetchNode(url, method, headersJson, bodyBase64, followRedirects) {
  const input = JSON.stringify({
    url,
    method,
    headers: JSON.parse(headersJson || "[]"),
    body_base64: bodyBase64 || null,
    follow_redirects: Boolean(followRedirects),
  });
  try {
    // Under Deno, the fetch-helper subprocess needs --allow-net to make
    // outbound requests. Extract the hostname and grant only that host.
    const host = new URL(url).hostname;
    const args = IS_DENO
      ? ["run", `--allow-net=${host}`, fetchHelperPath]
      : [fetchHelperPath];
    const out = execFileSync(
      process.execPath,
      args,
      { timeout: 30000, encoding: "utf-8", input, stdio: ["pipe", "pipe", "ignore"] },
    );
    return JSON.parse(out);
  } catch (e) {
    return { status: 0, headers: [], body_base64: "", error: e.message };
  }
}

function createNetworkStubsNode(moduleRef) {
  return {
    wasmsh_js_http_fetch(urlPtr, methodPtr, headersJsonPtr, bodyPtr, bodyLen, followRedirects) {
      const url = moduleRef.UTF8ToString(urlPtr);
      const method = moduleRef.UTF8ToString(methodPtr);
      const headersJson = moduleRef.UTF8ToString(headersJsonPtr);

      let bodyBase64 = "";
      if (bodyPtr !== 0 && bodyLen > 0) {
        const bodyBytes = new Uint8Array(moduleRef.HEAPU8.buffer, bodyPtr, bodyLen);
        bodyBase64 = Buffer.from(bodyBytes).toString("base64");
      }

      const result = syncHttpFetchNode(url, method, headersJson, bodyBase64, followRedirects);
      const resultJson = JSON.stringify(result);
      return moduleRef.stringToNewUTF8(resultJson);
    },
  };
}

// Module reference for network fetch closure. Set in createFullModule().
let _nodeModuleRef = null;

/**
 * Boot via Pyodide's standard `loadPyodide()` to get the full API
 * (runPythonAsync, loadPackage, pyimport, micropip support).
 *
 * We monkey-patch WebAssembly.instantiate to inject our wasmsh network
 * stubs and sentinel imports, then restore it after boot.
 */
export async function createFullModule(distDir) {
  // Polyfill __dirname/__filename for Deno — pyodide.asm.js needs them to
  // resolve its own location. Must point to the assets dir, not this module.
  const savedDirname = globalThis.__dirname;
  const savedFilename = globalThis.__filename;
  globalThis.__dirname = distDir;
  globalThis.__filename = resolve(distDir, "pyodide.asm.js");

  // Under Deno, pyodide.mjs's nodeLoadScript uses import() which loads
  // pyodide.asm.js as ESM — but the glue is CJS and calls require("fs").
  // Pre-load it via createRequire so _createPyodideModule is already
  // defined as a global, skipping nodeLoadScript entirely.
  if (IS_DENO && typeof globalThis._createPyodideModule === "undefined") {
    const denoRequire = createRequire(resolve(distDir, "package.json"));
    denoRequire("./pyodide.asm.js");
  }

  // Import loadPyodide from the packaged pyodide.mjs
  const pyodideMjs = resolve(distDir, "pyodide.mjs");
  const { loadPyodide } = await import(pathToFileURL(pyodideMjs).href);

  // Patch WebAssembly.instantiate to inject wasmsh imports
  const origInstantiate = WebAssembly.instantiate;
  const SENTINEL_MARKER = Symbol("wasmsh-sentinel");
  WebAssembly.instantiate = async function (binary, imports) {
    if (imports) {
      // Inject sentinel stubs
      if (!imports.sentinel) {
        imports.sentinel = {
          create_sentinel: () => SENTINEL_MARKER,
          is_sentinel: (value) => (value === SENTINEL_MARKER ? 1 : 0),
        };
      }
      // Inject wasmsh network fetch (replace Emscripten stubs too)
      if (!imports.env) imports.env = {};
      if (!imports.env.wasmsh_js_http_fetch || imports.env.wasmsh_js_http_fetch.stub) {
        imports.env.wasmsh_js_http_fetch = (...args) => {
          if (!_nodeModuleRef) return 0;
          return createNetworkStubsNode(_nodeModuleRef).wasmsh_js_http_fetch(...args);
        };
      }
    }
    return origInstantiate.call(this, binary, imports);
  };

  let pyodide;
  try {
    pyodide = await loadPyodide({
      indexURL: distDir + "/",
      // Suppress prompts and version check (our build ID won't match CDN)
      checkAPIVersion: false,
      _sysExecutable: "wasmsh-pyodide",
      args: [],
      env: { HOME: "/workspace", PYTHONHOME: "/" },
      stdout: () => {},
      stderr: () => {},
    });
  } finally {
    // Restore original WebAssembly.instantiate and CJS globals
    WebAssembly.instantiate = origInstantiate;
    globalThis.__dirname = savedDirname;
    globalThis.__filename = savedFilename;
  }

  // The underlying Emscripten module is accessible via pyodide._module
  const module = pyodide._module;
  if (!module || typeof module.ccall !== "function") {
    throw new Error("Pyodide module ccall not available");
  }

  _nodeModuleRef = module;
  module.FS.mkdirTree("/workspace");

  // Pre-load micropip so `import micropip` works immediately.
  // The wheel file is in the assets dir and indexed in pyodide-lock.json.
  try {
    await pyodide.loadPackage("micropip");
  } catch {
    // micropip not available in this dist — not fatal
  }

  // Attach the pyodide API to the module so the host can use it
  module._pyodide = pyodide;
  createRuntimeBridge(module);
  return module;
}
