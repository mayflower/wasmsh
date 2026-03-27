/**
 * Host wrapper: loads the custom Pyodide Emscripten module in Node
 * and exposes the raw Module for ccall/cwrap.
 *
 * Two modes:
 *   createProbeModule()      — lightweight, skips Python init
 *   createFullModule()       — boots Python interpreter (for FS tests)
 */
import { resolve, dirname } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const __dirname = dirname(fileURLToPath(import.meta.url));
const DIST = resolve(__dirname, "../../dist/pyodide-custom");

// Sentinel stubs matching src/core/sentinel.wat exports.
const SENTINEL_MARKER = Symbol("sentinel");
const sentinelStubs = {
  create_sentinel: () => SENTINEL_MARKER,
  is_sentinel: (v) => v === SENTINEL_MARKER ? 1 : 0,
};

function loadFactory() {
  const require = createRequire(import.meta.url);
  require(resolve(DIST, "pyodide.asm.js"));
  const factory = globalThis._createPyodideModule;
  if (typeof factory !== "function") {
    throw new Error("_createPyodideModule not found");
  }
  return factory;
}

function makeApi() {
  return {
    tests: [],
    config: { jsglobals: globalThis, indexURL: DIST },
    fatal_error: () => {},
    on_fatal: () => {},
    initializeStreams: () => {},
    finalizeBootstrap: () => {},
    version: "0.0.0",
    lockfile_info: {},
    loadBinaryFile: (path) => {
      const full = resolve(DIST, path);
      return existsSync(full) ? readFileSync(full) : new Uint8Array(0);
    },
    runtimeEnv: { IN_NODE: true, IN_BROWSER: false, IN_BROWSER_MAIN_THREAD: false },
  };
}

function makeInstantiateWasm() {
  return function instantiateWasm(imports, successCallback) {
    imports.sentinel = sentinelStubs;
    const wasmBytes = readFileSync(resolve(DIST, "pyodide.asm.wasm"));
    WebAssembly.instantiate(wasmBytes, imports).then(({ instance }) => {
      successCallback(instance, wasmBytes);
    });
    return {};
  };
}

/**
 * Lightweight module — onRuntimeInitialized capture, no Python.
 */
export async function createProbeModule() {
  const factory = loadFactory();

  let resolveModule;
  const modulePromise = new Promise((ok) => { resolveModule = ok; });

  factory({
    noInitialRun: true,
    thisProgram: "wasmsh-probe",
    locateFile: (path) => resolve(DIST, path),
    print: () => {},
    printErr: () => {},
    API: makeApi(),
    instantiateWasm: makeInstantiateWasm(),
    onRuntimeInitialized() { resolveModule(this); },
  }).catch(() => {});

  const mod = await modulePromise;
  if (typeof mod.ccall !== "function") throw new Error("Module.ccall not available");
  return mod;
}

/**
 * Full module — boots CPython interpreter so Python code can run.
 */
export async function createFullModule() {
  const factory = loadFactory();

  let resolveModule;
  const modulePromise = new Promise((ok) => { resolveModule = ok; });

  const logs = [];

  factory({
    noInitialRun: true,
    thisProgram: "wasmsh-probe",
    locateFile: (path) => resolve(DIST, path),
    print: (text) => logs.push(text),
    printErr: (text) => logs.push("[stderr] " + text),
    API: makeApi(),
    instantiateWasm: makeInstantiateWasm(),
    preRun: [(m) => {
      // Mount Python stdlib zip and set env BEFORE Python init.
      const stdlibZip = resolve(DIST, "python_stdlib.zip");
      if (existsSync(stdlibZip)) {
        const zipData = readFileSync(stdlibZip);
        m.FS.mkdirTree("/lib/python3.13");
        m.FS.writeFile("/lib/python3.13/python_stdlib.zip", zipData);
        m.ENV.PYTHONPATH = "/lib/python3.13/python_stdlib.zip";
        m.ENV.PYTHONHOME = "/";
      }
      m.FS.mkdirTree("/workspace");
    }],
    onRuntimeInitialized() { resolveModule(this); },
  }).catch(() => {});

  const mod = await modulePromise;
  if (typeof mod.ccall !== "function") throw new Error("Module.ccall not available");

  // Boot CPython. callMain([]) runs main() which inits the interpreter.
  // (preRun already mounted stdlib and set PYTHONPATH/PYTHONHOME.)
  mod.callMain([]);

  mod._logs = logs;
  return mod;
}
