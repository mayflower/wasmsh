// Browser Web Worker for wasmsh-pyodide.
//
// Uses Pyodide's standard loadPyodide() for boot and micropip for package
// installation — same approach as node-module.mjs. The only wasmsh-specific
// wiring is the sentinel + network fetch stubs injected via instantiate patch.

const decoder = new TextDecoder();

let bootPromise = null;
let moduleRef = null;
let pyodideRef = null;
let helperModulesPromise = null;
let protocolHelpers = null;
let runtimeBridgeHelpers = null;
let installHelpers = null;
let runtimeBridge = null;
let assetBaseUrl = null;
let sessionAllowedHosts = [];
/** Set of package names whose wheel files are actually served locally.
 *  Built at boot by batch HEAD-checking all lockfile entries in parallel. */
let bundledPackageNames = new Set();

function resolveHelperUrl(relativePath) {
  return new URL(relativePath, self.location.href).href;
}

async function ensureHelperModules() {
  if (!helperModulesPromise) {
    helperModulesPromise = Promise.all([
      import(resolveHelperUrl("./lib/protocol.mjs")),
      import(resolveHelperUrl("./lib/runtime-bridge.mjs")),
      import(resolveHelperUrl("./lib/install.mjs")),
    ]).then(([protocol, runtimeBridgeModule, install]) => {
      protocolHelpers = protocol;
      runtimeBridgeHelpers = runtimeBridgeModule;
      installHelpers = install;
    }).catch((error) => {
      helperModulesPromise = null;
      throw error;
    });
  }
  await helperModulesPromise;
}

function protocol() {
  if (!protocolHelpers) {
    throw new Error("protocol helpers not initialized");
  }
  return protocolHelpers;
}

function runtimeBridgeModule() {
  if (!runtimeBridgeHelpers) {
    throw new Error("runtime bridge helpers not initialized");
  }
  return runtimeBridgeHelpers;
}

/**
 * Synchronous HTTP fetch for wasmsh curl/wget (called from Rust via extern "C").
 *
 * Parameters are WASM pointers (C strings + byte buffer). Returns a pointer
 * to a JSON C string allocated with malloc. The Rust caller frees it.
 */
function createNetworkStubs(module) {
  return {
    wasmsh_js_http_fetch(urlPtr, methodPtr, headersJsonPtr, bodyPtr, bodyLen, followRedirects) {
      const url = module.UTF8ToString(urlPtr);
      const method = module.UTF8ToString(methodPtr);
      const headersJson = module.UTF8ToString(headersJsonPtr);

      let bodyBytes = null;
      if (bodyPtr !== 0 && bodyLen > 0) {
        bodyBytes = new Uint8Array(module.HEAPU8.buffer, bodyPtr, bodyLen).slice();
      }

      let result;
      try {
        const xhr = new XMLHttpRequest();
        xhr.open(method, url, false); // synchronous — works in Web Workers
        xhr.timeout = 30000;
        const headers = JSON.parse(headersJson || "[]");
        for (const [key, value] of headers) {
          xhr.setRequestHeader(key, value);
        }
        xhr.responseType = "arraybuffer";
        xhr.send(bodyBytes);

        const respHeaders = xhr
          .getAllResponseHeaders()
          .split("\r\n")
          .filter((h) => h)
          .map((h) => {
            const idx = h.indexOf(": ");
            return idx >= 0 ? [h.slice(0, idx), h.slice(idx + 2)] : [h, ""];
          });

        const respBytes = new Uint8Array(xhr.response || new ArrayBuffer(0));
        const bodyBase64 = protocol().encodeBase64(respBytes);

        result = JSON.stringify({
          status: xhr.status,
          headers: respHeaders,
          body_base64: bodyBase64,
        });
      } catch (e) {
        result = JSON.stringify({ status: 0, headers: [], body_base64: "", error: e.message });
      }

      return module.stringToNewUTF8(result);
    },
  };
}

function sendHostCommand(command) {
  if (!runtimeBridge) {
    throw new Error("runtime not initialized");
  }
  return runtimeBridge.sendHostCommand(command);
}

/**
 * Boot via Pyodide's standard loadPyodide() — same approach as node-module.mjs.
 *
 * We monkey-patch WebAssembly.instantiate to inject our wasmsh sentinel and
 * network fetch stubs, then restore it after boot. This avoids reimplementing
 * Pyodide's stdlib mounting, lockfile handling, and module initialization.
 */
async function boot(baseUrl) {
  await ensureHelperModules();
  assetBaseUrl = baseUrl.replace(/\/$/, "");

  // Dynamic import of pyodide.mjs works in classic workers (same as our
  // helper module imports above).
  const { loadPyodide } = await import(`${assetBaseUrl}/pyodide.mjs`);

  // Patch WebAssembly.instantiate to inject wasmsh imports
  const origInstantiate = WebAssembly.instantiate;
  const SENTINEL_MARKER = {};
  WebAssembly.instantiate = async function (binary, imports) {
    if (imports) {
      if (!imports.sentinel) {
        imports.sentinel = {
          create_sentinel: () => SENTINEL_MARKER,
          is_sentinel: (value) => (value === SENTINEL_MARKER ? 1 : 0),
        };
      }
      if (!imports.env) imports.env = {};
      if (!imports.env.wasmsh_js_http_fetch || imports.env.wasmsh_js_http_fetch.stub) {
        imports.env.wasmsh_js_http_fetch = (...args) => {
          if (!moduleRef) return 0;
          return createNetworkStubs(moduleRef).wasmsh_js_http_fetch(...args);
        };
      }
    }
    return origInstantiate.call(this, binary, imports);
  };

  let pyodide;
  try {
    pyodide = await loadPyodide({
      indexURL: assetBaseUrl + "/",
      checkAPIVersion: false,
      _sysExecutable: "wasmsh-pyodide",
      args: [],
      env: { HOME: "/workspace", PYTHONHOME: "/" },
      stdout: () => {},
      stderr: () => {},
    });
  } finally {
    WebAssembly.instantiate = origInstantiate;
  }

  const module = pyodide._module;
  if (!module || typeof module.ccall !== "function") {
    throw new Error("Pyodide module ccall not available");
  }

  moduleRef = module;
  pyodideRef = pyodide;
  module.FS.mkdirTree("/workspace");

  // Pre-load micropip
  try {
    await pyodide.loadPackage("micropip");
  } catch {
    // micropip not available — not fatal
  }

  // Build the set of bundled packages whose wheel files are actually served
  // locally.  We read the lockfile (the browser HTTP cache likely already has
  // it from loadPyodide) and then batch-HEAD all wheel file_names in parallel.
  // Only packages with a locally available wheel are considered "bundled".
  try {
    const lockUrl = `${assetBaseUrl}/pyodide-lock.json`;
    const resp = await fetch(lockUrl);
    if (resp.ok) {
      const lock = await resp.json();
      const entries = Object.entries(lock.packages || {});
      const checks = entries.map(async ([name, entry]) => {
        if (!entry.file_name) return null;
        try {
          const r = await fetch(`${assetBaseUrl}/${entry.file_name}`, { method: "HEAD" });
          return r.ok ? name : null;
        } catch {
          return null;
        }
      });
      const results = await Promise.all(checks);
      bundledPackageNames = new Set(results.filter(Boolean));
    } else {
      console.warn(`[wasmsh] Could not fetch bundled package index: HTTP ${resp.status}`);
      bundledPackageNames = new Set();
    }
  } catch (err) {
    console.warn(`[wasmsh] Failed to load bundled package index: ${err.message}`);
    bundledPackageNames = new Set();
  }

  module._pyodide = pyodide;
  runtimeBridge = runtimeBridgeModule().createRuntimeBridge(module);
}

async function ensureBooted(baseUrl) {
  if (!bootPromise) {
    bootPromise = boot(baseUrl).catch((err) => {
      bootPromise = null;
      throw err;
    });
  }
  await bootPromise;
}

const methods = {
  async init({
    assetBaseUrl: baseUrl,
    stepBudget = 0,
    initialFiles = [],
    allowedHosts = [],
  }) {
    await ensureBooted(baseUrl);
    sessionAllowedHosts = allowedHosts;
    const events = sendHostCommand({
      Init: { step_budget: stepBudget, allowed_hosts: allowedHosts },
    });
    for (const file of initialFiles) {
      sendHostCommand({
        WriteFile: {
          path: file.path,
          data: Array.from(protocol().decodeBase64(file.contentBase64)),
        },
      });
    }
    return { events, version: protocol().getVersion(events) };
  },

  async run({ command }) {
    if (pyodideRef) {
      const pipResult = await installHelpers.handlePipCommand(
        command, pyodideRef,
        (opts) => methods.installPythonPackages(opts),
      );
      if (pipResult) return pipResult;
    }
    const events = sendHostCommand({ Run: { input: command } });
    return protocol().buildRunResult(events);
  },

  async writeFile({ path, contentBase64 }) {
    const events = sendHostCommand({
      WriteFile: {
        path,
        data: Array.from(protocol().decodeBase64(contentBase64)),
      },
    });
    return { events };
  },

  async readFile({ path }) {
    const events = sendHostCommand({ ReadFile: { path } });
    return {
      events,
      contentBase64: protocol().encodeBase64(protocol().extractStream(events, "Stdout")),
    };
  },

  async listDir({ path }) {
    const events = sendHostCommand({ ListDir: { path } });
    return {
      events,
      output: decoder.decode(protocol().extractStream(events, "Stdout")),
    };
  },

  async installPythonPackages({ requirements, options = {} }) {
    const reqs = typeof requirements === "string" ? [requirements] : requirements;
    if (!Array.isArray(reqs)) {
      throw new Error("requirements must be a string or array of strings");
    }

    if (!pyodideRef) {
      throw new Error("Pyodide API not available — cannot install packages");
    }

    return installHelpers.installPackages(reqs, pyodideRef, {
      isBundled: (name) => bundledPackageNames.has(name),
      allowedHosts: sessionAllowedHosts,
      deps: options.deps,
    });
  },

  async close() {
    if (runtimeBridge) {
      runtimeBridge.close();
    }
    runtimeBridge = null;
    moduleRef = null;
    pyodideRef = null;
    bootPromise = null;
    bundledPackageNames = new Set();
    return { closed: true };
  },
};

self.onmessage = async (event) => {
  const request = event.data;
  try {
    if (!Object.hasOwn(methods, request.method)) {
      throw new Error(`unknown method: ${request.method}`);
    }
    const result = await methods[request.method](request.params ?? {});
    self.postMessage({ id: request.id, ok: true, result });
  } catch (error) {
    self.postMessage({
      id: request.id ?? null,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
};
