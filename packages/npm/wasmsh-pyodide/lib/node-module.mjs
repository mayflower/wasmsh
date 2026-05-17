import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { buildBaselineBootPlan } from "./baseline/boot-plan.mjs";
import { composeWasmImports } from "./baseline/import-composer.mjs";
import { assertOfflineBaselineBootPlan } from "./baseline/offline-guard.mjs";
import { captureScopedGlobals, restoreScopedGlobals } from "./baseline/sandbox-globals.mjs";

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

// Hard ceiling on the fetch-helper subprocess wall-clock. The inner
// AbortController-driven timeout in fetch-helper.mjs honors the caller's
// `timeout_ms`; this outer execFileSync timeout is the last-ditch kill for
// a runaway helper and only fires if the inner timeout fails. Keep it
// strictly larger than any expected per-request timeout_ms.
const HELPER_OUTER_TIMEOUT_MS = 60000;

export function syncHttpFetchNode(
  url,
  method,
  headersJson,
  bodyBase64,
  followRedirects,
  optionsObj,
) {
  const input = JSON.stringify({
    url,
    method,
    headers: JSON.parse(headersJson || "[]"),
    body_base64: bodyBase64 || null,
    follow_redirects: Boolean(followRedirects),
    options: optionsObj || {},
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
      {
        timeout: HELPER_OUTER_TIMEOUT_MS,
        encoding: "utf-8",
        input,
        stdio: ["pipe", "pipe", "ignore"],
      },
    );
    return JSON.parse(out);
  } catch (e) {
    return { status: 0, headers: [], body_base64: "", error: e.message };
  }
}

function parseOptionsJsonPtr(moduleRef, optionsPtr) {
  if (!optionsPtr) return {};
  try {
    const raw = moduleRef.UTF8ToString(optionsPtr);
    if (!raw) return {};
    const obj = JSON.parse(raw);
    return obj && typeof obj === "object" ? obj : {};
  } catch {
    return {};
  }
}

function createNetworkStubsNode(moduleRef, fetchHandlerSync) {
  // fetchHandlerSync defaults to the in-tree helper-spawning broker so
  // callers can override it (tests, dispatcher-side mocks) — earlier code
  // accepted the argument but ignored it, which silently bypassed any
  // test-side network policy. Honor it here.
  const handler = fetchHandlerSync || syncHttpFetchNode;
  return {
    wasmsh_js_http_fetch(
      urlPtr,
      methodPtr,
      headersJsonPtr,
      bodyPtr,
      bodyLen,
      followRedirects,
      optionsPtr,
    ) {
      const url = moduleRef.UTF8ToString(urlPtr);
      const method = moduleRef.UTF8ToString(methodPtr);
      const headersJson = moduleRef.UTF8ToString(headersJsonPtr);
      const optionsObj = parseOptionsJsonPtr(moduleRef, optionsPtr);

      let bodyBase64 = "";
      if (bodyPtr !== 0 && bodyLen > 0) {
        const bodyBytes = new Uint8Array(moduleRef.HEAPU8.buffer, bodyPtr, bodyLen);
        bodyBase64 = Buffer.from(bodyBytes).toString("base64");
      }

      const result = handler(
        url,
        method,
        headersJson,
        bodyBase64,
        followRedirects,
        optionsObj,
      );
      const resultJson = JSON.stringify(result);
      return moduleRef.stringToNewUTF8(resultJson);
    },
  };
}

function createScopedInstantiateWasm(fetchHandlerSync, moduleRefAccessor, {
  compiledWasmModule = null,
  wasmBytes = null,
} = {}) {
  const SENTINEL_MARKER = Symbol("wasmsh-sentinel");
  return (info, successCallback) => {
    const imports = composeWasmImports({
      imports: info,
      env: {
        wasmsh_js_http_fetch: (...args) => {
          const moduleRef = moduleRefAccessor();
          if (!moduleRef) {
            return 0;
          }
          return createNetworkStubsNode(moduleRef, fetchHandlerSync).wasmsh_js_http_fetch(...args);
        },
      },
      sentinel: {
        create_sentinel: () => SENTINEL_MARKER,
        is_sentinel: (value) => (value === SENTINEL_MARKER ? 1 : 0),
      },
    });
    if (compiledWasmModule) {
      WebAssembly.instantiate(compiledWasmModule, imports).then(
        (instance) => successCallback(instance, compiledWasmModule),
      );
      return {};
    }
    const wasmSource = wasmBytes ?? readFileSync(resolve(globalThis.__dirname, "pyodide.asm.wasm"));
    WebAssembly.instantiate(wasmSource, imports).then(
      ({ instance, module }) => successCallback(instance, module),
    );
    return {};
  };
}

async function loadModuleWithBaseline(distDir, {
  snapshotBytes = null,
  fetchHandlerSync = syncHttpFetchNode,
  makeSnapshot = false,
  compiledWasmModule = null,
  wasmBytes = null,
} = {}) {
  const bootPlan = assertOfflineBaselineBootPlan(
    buildBaselineBootPlan({ assetDir: distDir }),
  );

  // Polyfill __dirname/__filename for Deno — pyodide.asm.js needs them to
  // resolve its own location. Must point to the assets dir, not this module.
  const savedGlobals = captureScopedGlobals(["__dirname", "__filename", "require"]);
  globalThis.__dirname = distDir;
  globalThis.__filename = resolve(distDir, "pyodide.asm.js");

  const requireForAssets = createRequire(resolve(distDir, "package.json"));
  requireForAssets("./pyodide.asm.js");
  const originalFactory = globalThis._createPyodideModule;
  if (typeof originalFactory !== "function") {
    throw new Error("_createPyodideModule not found");
  }
  let moduleRef = null;
  globalThis._createPyodideModule = (settings) => originalFactory({
    ...settings,
    instantiateWasm: createScopedInstantiateWasm(fetchHandlerSync, () => moduleRef, {
      compiledWasmModule,
      wasmBytes,
    }),
  });

  const pyodideMjs = resolve(distDir, "pyodide.mjs");
  const { loadPyodide } = await import(pathToFileURL(pyodideMjs).href);

  let pyodide;
  try {
    pyodide = await loadPyodide({
      indexURL: `${bootPlan.assetDir}/`,
      // Suppress prompts and version check (our build ID won't match CDN)
      checkAPIVersion: false,
      _sysExecutable: "wasmsh-pyodide",
      args: [],
      env: {
        HOME: "/workspace",
        PYTHONHOME: "/",
        PYTHONHASHSEED: "0",
      },
      stdout: () => {},
      stderr: () => {},
      ...(makeSnapshot ? { _makeSnapshot: true } : {}),
      ...(snapshotBytes ? { _loadSnapshot: snapshotBytes } : {}),
    });
  } finally {
    globalThis._createPyodideModule = originalFactory;
    restoreScopedGlobals(savedGlobals);
  }

  // The underlying Emscripten module is accessible via pyodide._module
  const module = pyodide._module;
  if (!module || typeof module.ccall !== "function") {
    throw new Error("Pyodide module ccall not available");
  }

  moduleRef = module;
  module.FS.mkdirTree("/workspace");

  module._pyodide = pyodide;
  return module;
}

export async function createFullModule(distDir, options = {}) {
  return loadModuleWithBaseline(distDir, options);
}

export async function createRestoredModuleFromSnapshot(distDir, snapshotBytes, options = {}) {
  if (!snapshotBytes) {
    throw new Error("snapshotBytes are required");
  }
  return loadModuleWithBaseline(distDir, {
    ...options,
    snapshotBytes,
  });
}
