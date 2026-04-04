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
import { execFileSync } from "node:child_process";
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

// Module reference for the network fetch closure (set after boot).
let _moduleRef = null;

const fetchHelperPath = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../packages/npm/wasmsh-pyodide/lib/fetch-helper.mjs",
);

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

function makeInstantiateWasm() {
  return function instantiateWasm(imports, successCallback) {
    imports.sentinel = sentinelStubs;
    if (!imports.env) {
      imports.env = {};
    }
    imports.env.wasmsh_js_http_fetch = (urlPtr, methodPtr, headersJsonPtr, bodyPtr, bodyLen, followRedirects) => {
      if (!_moduleRef) return 0;
      const url = _moduleRef.UTF8ToString(urlPtr);
      const method = _moduleRef.UTF8ToString(methodPtr);
      const headersJson = _moduleRef.UTF8ToString(headersJsonPtr);
      let bodyBase64 = "";
      if (bodyPtr !== 0 && bodyLen > 0) {
        const bodyBytes = new Uint8Array(_moduleRef.HEAPU8.buffer, bodyPtr, bodyLen);
        bodyBase64 = Buffer.from(bodyBytes).toString("base64");
      }
      const result = syncHttpFetchNode(url, method, headersJson, bodyBase64, followRedirects);
      return _moduleRef.stringToNewUTF8(JSON.stringify(result));
    };
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
  _moduleRef = mod;
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
      m.FS.mkdirTree("/lib/python3.13/site-packages");
      m.FS.mkdirTree("/workspace");
    }],
    onRuntimeInitialized() { resolveModule(this); },
  }).catch(() => {});

  const mod = await modulePromise;
  if (typeof mod.ccall !== "function") throw new Error("Module.ccall not available");
  _moduleRef = mod;

  // Boot CPython. callMain([]) runs main() which inits the interpreter.
  // (preRun already mounted stdlib and set PYTHONPATH/PYTHONHOME.)
  mod.callMain([]);

  mod._logs = logs;
  return mod;
}
