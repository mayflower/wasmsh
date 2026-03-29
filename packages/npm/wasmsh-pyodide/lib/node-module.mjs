import { createRequire } from "node:module";
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

function makeInstantiateWasm(distDir, onError) {
  return function instantiateWasm(imports, successCallback) {
    imports.sentinel = sentinelStubs;
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

  module.callMain([]);
  module._logs = logs;
  return module;
}
