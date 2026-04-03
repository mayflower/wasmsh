import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

const SENTINEL_MARKER = Symbol("wasmsh-sentinel");
const sentinelStubs = {
  create_sentinel: () => SENTINEL_MARKER,
  is_sentinel: (value) => (value === SENTINEL_MARKER ? 1 : 0),
};

function loadFactory(distDir) {
  const require = createRequire(import.meta.url);
  require(resolve(distDir, "pyodide.asm.js"));
  const factory = globalThis._createPyodideModule;
  if (typeof factory !== "function") {
    throw new Error("_createPyodideModule not found");
  }
  return factory;
}

function makeApi(distDir) {
  return {
    tests: [],
    config: { jsglobals: globalThis, indexURL: distDir },
    fatal_error: () => {},
    on_fatal: () => {},
    initializeStreams: () => {},
    finalizeBootstrap: () => {},
    version: "0.0.0",
    lockfile_info: {},
    loadBinaryFile: (filePath) => {
      const full = resolve(distDir, filePath);
      if (!existsSync(full)) {
        throw new Error(`Required Pyodide asset not found: ${full}`);
      }
      return readFileSync(full);
    },
    runtimeEnv: {
      IN_NODE: true,
      IN_BROWSER: false,
      IN_BROWSER_MAIN_THREAD: false,
    },
  };
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
function syncHttpFetchNode(url, method, headersJson, bodyBase64, followRedirects) {
  const input = JSON.stringify({
    url,
    method,
    headers: JSON.parse(headersJson || "[]"),
    body_base64: bodyBase64 || null,
    follow_redirects: Boolean(followRedirects),
  });
  try {
    const out = execFileSync(
      process.execPath,
      [fetchHelperPath],
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

function makeInstantiateWasm(distDir, onError) {
  return function instantiateWasm(imports, successCallback) {
    imports.sentinel = sentinelStubs;
    // Provide network fetch in env namespace (Emscripten default for extern "C").
    // Uses the module-level _nodeModuleRef which is set after boot.
    if (!imports.env) {
      imports.env = {};
    }
    imports.env.wasmsh_js_http_fetch = (...args) => {
      if (!_nodeModuleRef) return 0;
      return createNetworkStubsNode(_nodeModuleRef).wasmsh_js_http_fetch(...args);
    };
    const wasmBytes = readFileSync(resolve(distDir, "pyodide.asm.wasm"));
    WebAssembly.instantiate(wasmBytes, imports)
      .then(({ instance }) => {
        successCallback(instance, wasmBytes);
      })
      .catch(onError);
    return {};
  };
}

export async function createFullModule(distDir) {
  const factory = loadFactory(distDir);

  let resolveModule;
  let rejectModule;
  const modulePromise = new Promise((resolvePromise, rejectPromise) => {
    resolveModule = resolvePromise;
    rejectModule = rejectPromise;
  });

  const logs = [];

  factory({
    noInitialRun: true,
    thisProgram: "wasmsh-pyodide",
    locateFile: (path) => resolve(distDir, path),
    print: (text) => logs.push(text),
    printErr: (text) => logs.push(`[stderr] ${text}`),
    API: makeApi(distDir),
    instantiateWasm: makeInstantiateWasm(distDir, rejectModule),
    preRun: [
      (module) => {
        const stdlibZip = resolve(distDir, "python_stdlib.zip");
        if (existsSync(stdlibZip)) {
          const zipData = readFileSync(stdlibZip);
          module.FS.mkdirTree("/lib/python3.13");
          module.FS.writeFile("/lib/python3.13/python_stdlib.zip", zipData);
          module.ENV.PYTHONPATH = "/lib/python3.13/python_stdlib.zip";
          module.ENV.PYTHONHOME = "/";
        }
        module.FS.mkdirTree("/lib/python3.13/site-packages");
        module.FS.mkdirTree("/workspace");
      },
    ],
    onRuntimeInitialized() {
      resolveModule(this);
    },
  }).catch((err) => {
    rejectModule(err);
  });

  const module = await modulePromise;
  if (typeof module.ccall !== "function") {
    throw new Error("Module.ccall not available");
  }

  // Bind the module reference so network fetch stubs can access WASM memory.
  _nodeModuleRef = module;

  module.callMain([]);
  module._logs = logs;
  return module;
}
