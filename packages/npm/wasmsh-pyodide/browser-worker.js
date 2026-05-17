// Browser Web Worker for wasmsh-pyodide.
//
// Uses Pyodide's standard loadPyodide() for boot and micropip for package
// installation — same approach as node-module.mjs. Baseline boot stays
// offline/deterministic; package resolution remains a session-time concern.

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
/** Map of package name -> wheel file from pyodide-lock.json. */
let bundledPackageFiles = new Map();
/** Cache of package name -> locally served status. */
let bundledPackageAvailability = new Map();
let bundledPackageIndexPromise = null;

function resolveHelperUrl(relativePath) {
  return new URL(relativePath, self.location.href).href;
}

let fetchMembraneHelpers = null;

async function ensureHelperModules() {
  if (!helperModulesPromise) {
    helperModulesPromise = Promise.all([
      import(resolveHelperUrl("./lib/protocol.mjs")),
      import(resolveHelperUrl("./lib/runtime-bridge.mjs")),
      import(resolveHelperUrl("./lib/install.mjs")),
      import(resolveHelperUrl("./lib/fetch-membrane.mjs")),
    ]).then(([protocol, runtimeBridgeModule, install, membrane]) => {
      protocolHelpers = protocol;
      runtimeBridgeHelpers = runtimeBridgeModule;
      installHelpers = install;
      fetchMembraneHelpers = membrane;
    }).catch((error) => {
      helperModulesPromise = null;
      throw error;
    });
  }
  await helperModulesPromise;
}

async function ensureBundledPackageIndex() {
  if (bundledPackageIndexPromise) {
    await bundledPackageIndexPromise;
    return;
  }
  bundledPackageIndexPromise = (async () => {
    const lockUrl = `${assetBaseUrl}/pyodide-lock.json`;
    const resp = await fetch(lockUrl);
    if (!resp.ok) {
      throw new Error(`Could not fetch bundled package index: HTTP ${resp.status}`);
    }
    const lock = await resp.json();
    bundledPackageFiles = new Map();
    for (const [name, entry] of Object.entries(lock.packages || {})) {
      if (entry && entry.file_name) {
        bundledPackageFiles.set(name, entry.file_name);
      }
    }
  })().catch((error) => {
    bundledPackageIndexPromise = null;
    throw error;
  });
  await bundledPackageIndexPromise;
}

async function isBundledPackage(name) {
  if (bundledPackageAvailability.has(name)) {
    return bundledPackageAvailability.get(name);
  }

  try {
    await ensureBundledPackageIndex();
  } catch (error) {
    console.warn(`[wasmsh] Failed to load bundled package index: ${error.message}`);
    bundledPackageAvailability.set(name, false);
    return false;
  }

  const fileName = bundledPackageFiles.get(name);
  if (!fileName) {
    bundledPackageAvailability.set(name, false);
    return false;
  }

  try {
    const response = await fetch(`${assetBaseUrl}/${fileName}`, { method: "HEAD" });
    const available = response.ok;
    bundledPackageAvailability.set(name, available);
    return available;
  } catch {
    bundledPackageAvailability.set(name, false);
    return false;
  }
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
// Default cap on response body size when no per-request cap is supplied.
// Matches the Rust-side DECOMPRESS_OUTPUT_LIMIT so the worker never holds
// more than ~64 MiB of fetch output regardless of caller config.
const DEFAULT_MAX_RESPONSE_BYTES = 64 * 1024 * 1024;
const DEFAULT_FETCH_TIMEOUT_MS = 30000;

function parseFetchOptions(module, optionsPtr) {
  if (!optionsPtr) {
    return {};
  }
  try {
    const raw = module.UTF8ToString(optionsPtr);
    if (!raw) return {};
    const obj = JSON.parse(raw);
    return obj && typeof obj === "object" ? obj : {};
  } catch {
    return {};
  }
}

function createNetworkStubs(module) {
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
      const url = module.UTF8ToString(urlPtr);
      const method = module.UTF8ToString(methodPtr);
      const headersJson = module.UTF8ToString(headersJsonPtr);
      const opts = parseFetchOptions(module, optionsPtr);
      const timeoutMs =
        typeof opts.timeout_ms === "number" && opts.timeout_ms > 0
          ? opts.timeout_ms
          : DEFAULT_FETCH_TIMEOUT_MS;
      const maxResponseBytes =
        typeof opts.max_response_bytes === "number" && opts.max_response_bytes > 0
          ? opts.max_response_bytes
          : DEFAULT_MAX_RESPONSE_BYTES;

      let bodyBytes = null;
      if (bodyPtr !== 0 && bodyLen > 0) {
        bodyBytes = new Uint8Array(module.HEAPU8.buffer, bodyPtr, bodyLen).slice();
      }

      let result;
      try {
        const xhr = new XMLHttpRequest();
        xhr.open(method, url, false); // synchronous — works in Web Workers
        xhr.timeout = timeoutMs;
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
        if (respBytes.byteLength > maxResponseBytes) {
          result = JSON.stringify({
            status: 0,
            headers: [],
            body_base64: "",
            error: `response exceeds max_response_bytes (${respBytes.byteLength} > ${maxResponseBytes})`,
          });
          return module.stringToNewUTF8(result);
        }
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
 */
async function boot(baseUrl) {
  await ensureHelperModules();
  assetBaseUrl = baseUrl.replace(/\/$/, "");

  // Dynamic import of pyodide.mjs works in classic workers (same as our
  // helper module imports above).
  const { loadPyodide } = await import(`${assetBaseUrl}/pyodide.mjs`);

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
    // No global boot-time monkey patches to clean up.
  }

  const module = pyodide._module;
  if (!module || typeof module.ccall !== "function") {
    throw new Error("Pyodide module ccall not available");
  }

  moduleRef = module;
  pyodideRef = pyodide;
  module.FS.mkdirTree("/workspace");

  // Hook Emscripten FS for the per-file / total-bytes / inode quotas.
  // Shell-via-EmscriptenFs and direct Python `open()` both reach this
  // FS object, so the membrane catches both. Audit F6.
  if (fetchMembraneHelpers) {
    // fetch-membrane and fs-membrane are imported by the same
    // ensureHelperModules promise (both in `./lib/`); load fs-membrane
    // lazily here so the import surface stays small.
    const { installFsMembrane } = await import(resolveHelperUrl("./lib/fs-membrane.mjs"));
    installFsMembrane(module.FS);
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
    // Install the JS fetch membrane on this worker's `self` global BEFORE
    // any Init/Run reaches Pyodide. Pyodide's `js` proxy resolves
    // `js.fetch`, `pyodide.http.pyfetch`, and `micropip`'s HTTP through
    // self.fetch, so all three are now gated by the same allowlist as
    // curl. Audit F2: this browser path was previously protected only by
    // the (Python-globals-exposed, hence bypassable) Python preamble.
    if (fetchMembraneHelpers) {
      fetchMembraneHelpers.installFetchMembrane(self, allowedHosts);
    }
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
      isBundled: isBundledPackage,
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
    bundledPackageFiles = new Map();
    bundledPackageAvailability = new Map();
    bundledPackageIndexPromise = null;
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
