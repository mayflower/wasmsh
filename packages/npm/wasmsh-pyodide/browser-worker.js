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
let allowlistHelpers = null;
let runtimeBridgeHelpers = null;
let runtimeBridge = null;
let assetBaseUrl = null;
let sessionAllowedHosts = [];
let bundledPackageNames = null;

function resolveHelperUrl(relativePath) {
  return new URL(relativePath, self.location.href).href;
}

async function ensureHelperModules() {
  if (!helperModulesPromise) {
    helperModulesPromise = Promise.all([
      import(resolveHelperUrl("./lib/protocol.mjs")),
      import(resolveHelperUrl("./lib/allowlist.mjs")),
      import(resolveHelperUrl("./lib/runtime-bridge.mjs")),
    ]).then(([protocol, allowlist, runtimeBridgeModule]) => {
      protocolHelpers = protocol;
      allowlistHelpers = allowlist;
      runtimeBridgeHelpers = runtimeBridgeModule;
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

function allowlist() {
  if (!allowlistHelpers) {
    throw new Error("allowlist helpers not initialized");
  }
  return allowlistHelpers;
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

  // Cache bundled package names from the lockfile (already loaded by Pyodide).
  try {
    const resp = await fetch(`${assetBaseUrl}/pyodide-lock.json`);
    if (resp.ok) {
      const lock = await resp.json();
      bundledPackageNames = new Set(Object.keys(lock.packages || {}));
    }
  } catch {
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
    // Intercept pip/pip3 install commands and route through micropip.
    const pipMatch = command.match(
      /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+install\s+(.+)$/,
    );
    if (pipMatch) {
      return this._handlePipInstall(pipMatch[1]);
    }
    const events = sendHostCommand({ Run: { input: command } });
    return protocol().buildRunResult(events);
  },

  async _handlePipInstall(argsStr) {
    const packages = argsStr
      .split(/\s+/)
      .filter((a) => a && !a.startsWith("-"));
    if (packages.length === 0) {
      return {
        events: [],
        stdout: "",
        stderr: "Usage: pip install <package> [package ...]\n",
        output: "Usage: pip install <package> [package ...]\n",
        exitCode: 1,
      };
    }
    try {
      await methods.installPythonPackages({ requirements: packages });
      const msg = packages
        .map((p) => `Successfully installed ${p}`)
        .join("\n") + "\n";
      return { events: [], stdout: msg, stderr: "", output: msg, exitCode: 0 };
    } catch (err) {
      const msg = `ERROR: ${err.message}\n`;
      return { events: [], stdout: "", stderr: msg, output: msg, exitCode: 1 };
    }
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

    const bundled = bundledPackageNames || new Set();
    const installed = [];
    for (const req of reqs) {
      if (/^file:/i.test(req)) {
        throw new Error(`file: URIs are not supported for security: ${req}`);
      }

      // Bundled packages: resolve offline via pyodide.loadPackage()
      const isPlainName = !req.startsWith("emfs:") && !/^https?:\/\//i.test(req);
      if (isPlainName && bundled.has(req)) {
        await pyodideRef.loadPackage(req);
        installed.push({ requirement: req });
        continue;
      }

      if (/^https?:\/\//i.test(req) && !allowlist().isHostAllowed(req, sessionAllowedHosts)) {
        throw new Error(
          `Host not allowed for package install: ${req}. ` +
          "Configure allowedHosts when creating the session.",
        );
      }
      if (isPlainName && sessionAllowedHosts.length === 0) {
        throw new Error(
          `Package name installs require network access: ${req}. ` +
          "Configure allowedHosts (e.g., ['cdn.jsdelivr.net', 'pypi.org', 'files.pythonhosted.org']) when creating the session.",
        );
      }

      const micropip = pyodideRef.pyimport("micropip");
      await micropip.install(req, { deps: options.deps !== false });
      installed.push({ requirement: req });
    }
    return { installed, requirements: reqs };
  },

  async close() {
    if (runtimeBridge) {
      runtimeBridge.close();
    }
    runtimeBridge = null;
    moduleRef = null;
    pyodideRef = null;
    bootPromise = null;
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
