// Browser workers loaded via importScripts() cannot use ES module imports.
// Inline the protocol helpers here (canonical versions in lib/protocol.mjs).

const decoder = new TextDecoder();

function extractStream(events, key) {
  const chunks = [];
  let total = 0;
  for (const event of events) {
    if (event && typeof event === "object" && key in event) {
      const chunk = event[key];
      chunks.push(chunk);
      total += chunk.length;
    }
  }
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

function getExitCode(events) {
  for (const event of events) {
    if (event && typeof event === "object" && "Exit" in event) {
      return event.Exit;
    }
  }
  return null;
}

function getVersion(events) {
  for (const event of events) {
    if (event && typeof event === "object" && "Version" in event) {
      return event.Version;
    }
  }
  return null;
}

function encodeBase64(bytes) {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

function decodeBase64(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function buildRunResult(events) {
  const stdout = decoder.decode(extractStream(events, "Stdout"));
  const stderr = decoder.decode(extractStream(events, "Stderr"));
  return {
    events,
    stdout,
    stderr,
    output: stdout + stderr,
    exitCode: getExitCode(events),
  };
}

let bootPromise = null;
let moduleRef = null;
let runtimeHandle = null;
let assetBaseUrl = null;
let sessionAllowedHosts = [];

/**
 * Check if a URL's host is in the allowlist.
 * Mirrors the Rust HostAllowlist semantics.
 */
function isHostAllowed(url, allowedHosts) {
  if (!allowedHosts || allowedHosts.length === 0) return false;
  let parsed;
  try { parsed = new URL(url); } catch { return false; }
  const host = parsed.hostname.toLowerCase();
  const port = parsed.port ? Number(parsed.port) : null;
  for (const pattern of allowedHosts) {
    const colonIdx = pattern.lastIndexOf(":");
    let patHost, patPort;
    if (colonIdx > 0 && /^\d+$/.test(pattern.slice(colonIdx + 1))) {
      patHost = pattern.slice(0, colonIdx).toLowerCase();
      patPort = Number(pattern.slice(colonIdx + 1));
    } else {
      patHost = pattern.toLowerCase();
      patPort = null;
    }
    if (patPort !== null && port !== patPort) continue;
    if (patHost.startsWith("*.")) {
      const suffix = patHost.slice(2);
      if (host === suffix || host.endsWith(`.${suffix}`)) return true;
    } else {
      if (host === patHost) return true;
    }
  }
  return false;
}

const SENTINEL_MARKER = {};
const sentinelStubs = {
  create_sentinel: () => SENTINEL_MARKER,
  is_sentinel: (value) => (value === SENTINEL_MARKER ? 1 : 0),
};

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

        // Encode response body as base64
        const respBytes = new Uint8Array(xhr.response || new ArrayBuffer(0));
        const bodyBase64 = encodeBase64(respBytes);

        result = JSON.stringify({
          status: xhr.status,
          headers: respHeaders,
          body_base64: bodyBase64,
        });
      } catch (e) {
        result = JSON.stringify({ status: 0, headers: [], body_base64: "", error: e.message });
      }

      // Allocate a C string via malloc and copy the result
      const resultPtr = module.stringToNewUTF8(result);
      return resultPtr;
    },
  };
}

function sendHostCommand(command) {
  const json = JSON.stringify(command);
  const jsonPtr = moduleRef.stringToNewUTF8(json);
  const resultPtr = moduleRef.ccall(
    "wasmsh_runtime_handle_json",
    "number",
    ["number", "number"],
    [runtimeHandle, jsonPtr],
  );
  moduleRef._free(jsonPtr);
  const resultStr = moduleRef.UTF8ToString(resultPtr);
  moduleRef.ccall("wasmsh_runtime_free_string", null, ["number"], [resultPtr]);
  return JSON.parse(resultStr);
}

async function boot(baseUrl) {
  assetBaseUrl = baseUrl.replace(/\/$/, "");
  importScripts(`${assetBaseUrl}/pyodide.asm.js`);
  const factory = self._createPyodideModule;
  if (typeof factory !== "function") {
    throw new Error("_createPyodideModule not found");
  }

  let stdlibBytes = null;
  const stdlibResponse = await fetch(`${assetBaseUrl}/python_stdlib.zip`);
  if (stdlibResponse.ok) {
    stdlibBytes = new Uint8Array(await stdlibResponse.arrayBuffer());
  }

  // Fetch bundled wheel files (micropip, packaging) and lockfile
  const bundledWheels = [];
  let lockfileBytes = null;
  try {
    const lockResp = await fetch(`${assetBaseUrl}/pyodide-lock.json`);
    if (lockResp.ok) {
      lockfileBytes = new Uint8Array(await lockResp.arrayBuffer());
      const lockData = JSON.parse(new TextDecoder().decode(lockfileBytes));
      // Fetch micropip + packaging wheels listed in lockfile
      for (const name of ["micropip", "packaging"]) {
        const pkg = lockData.packages?.[name];
        if (pkg?.file_name) {
          const whlResp = await fetch(`${assetBaseUrl}/${pkg.file_name}`);
          if (whlResp.ok) {
            bundledWheels.push({
              name: pkg.file_name,
              data: new Uint8Array(await whlResp.arrayBuffer()),
            });
          }
        }
      }
    }
  } catch { /* lockfile or wheels unavailable */ }

  moduleRef = await new Promise((resolve, reject) => {
    factory({
      noInitialRun: true,
      thisProgram: "wasmsh-pyodide",
      locateFile(path) {
        return `${assetBaseUrl}/${path}`;
      },
      print() {},
      printErr() {},
      API: {
        tests: [],
        config: { jsglobals: self, indexURL: `${assetBaseUrl}/` },
        fatal_error() {},
        on_fatal() {},
        initializeStreams() {},
        finalizeBootstrap() {},
        version: "0.0.0",
        lockfile_info: {},
        loadBinaryFile(path) {
          throw new Error(`Synchronous loadBinaryFile unavailable for ${path}`);
        },
        runtimeEnv: {
          IN_NODE: false,
          IN_BROWSER: true,
          IN_BROWSER_MAIN_THREAD: false,
          IN_BROWSER_WEB_WORKER: true,
        },
      },
      instantiateWasm(imports, successCallback) {
        imports.sentinel = sentinelStubs;
        // Provide network fetch to Emscripten env namespace.
        // Uses the outer `moduleRef` which is set before any shell commands run.
        if (!imports.env) {
          imports.env = {};
        }
        imports.env.wasmsh_js_http_fetch = (...args) => {
          if (!moduleRef) return 0;
          return createNetworkStubs(moduleRef).wasmsh_js_http_fetch(...args);
        };
        fetch(`${assetBaseUrl}/pyodide.asm.wasm`)
          .then((response) => response.arrayBuffer())
          .then((buffer) =>
            WebAssembly.instantiate(buffer, imports).then((result) => {
              successCallback(result.instance, new Uint8Array(buffer));
            }),
          )
          .catch(reject);
        return {};
      },
      preRun: [
        (module) => {
          if (stdlibBytes) {
            module.FS.mkdirTree("/lib/python3.13");
            module.FS.writeFile("/lib/python3.13/python_stdlib.zip", stdlibBytes);
            module.ENV.PYTHONPATH = "/lib/python3.13/python_stdlib.zip";
            module.ENV.PYTHONHOME = "/";
          }
          module.FS.mkdirTree("/lib/python3.13/site-packages");

          // Mount lockfile
          if (lockfileBytes) {
            module.FS.writeFile("/lib/pyodide-lock.json", lockfileBytes);
          }

          // Mount bundled wheels and add to PYTHONPATH
          const wheelPaths = [];
          for (const whl of bundledWheels) {
            const fsPath = `/lib/python3.13/${whl.name}`;
            module.FS.writeFile(fsPath, whl.data);
            wheelPaths.push(fsPath);
          }
          if (wheelPaths.length > 0) {
            module.ENV.PYTHONPATH = [
              "/lib/python3.13/python_stdlib.zip",
              "/lib/python3.13/site-packages",
              ...wheelPaths,
            ].join(":");
          }

          module.FS.mkdirTree("/workspace");
        },
      ],
      onRuntimeInitialized() {
        resolve(this);
      },
    }).catch(reject);
  });

  moduleRef.callMain([]);
  runtimeHandle = moduleRef.ccall("wasmsh_runtime_new", "number", [], []);
}

async function ensureBooted(baseUrl) {
  if (!bootPromise) {
    bootPromise = boot(baseUrl).catch((err) => {
      bootPromise = null; // allow retry on transient failures
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
          data: Array.from(decodeBase64(file.contentBase64)),
        },
      });
    }
    return { events, version: getVersion(events) };
  },

  async run({ command }) {
    const events = sendHostCommand({ Run: { input: command } });
    return buildRunResult(events);
  },

  async writeFile({ path, contentBase64 }) {
    const events = sendHostCommand({
      WriteFile: {
        path,
        data: Array.from(decodeBase64(contentBase64)),
      },
    });
    return { events };
  },

  async readFile({ path }) {
    const events = sendHostCommand({ ReadFile: { path } });
    return {
      events,
      contentBase64: encodeBase64(extractStream(events, "Stdout")),
    };
  },

  async listDir({ path }) {
    const events = sendHostCommand({ ListDir: { path } });
    return {
      events,
      output: decoder.decode(extractStream(events, "Stdout")),
    };
  },

  async installPythonPackages({ requirements, options = {} }) {
    const reqs = typeof requirements === "string" ? [requirements] : requirements;
    if (!Array.isArray(reqs)) {
      throw new Error("requirements must be a string or array of strings");
    }

    function extractWheel(wheelPath) {
      const events = sendHostCommand({
        Run: {
          input: `python3 << 'WASMSH_PIP_EOF'
import zipfile, os, sys
sp = '/lib/python3.13/site-packages'
os.makedirs(sp, exist_ok=True)
if sp not in sys.path:
    sys.path.insert(0, sp)
whl = '${wheelPath.replace(/'/g, "'\\''")}'
if not os.path.isfile(whl):
    print('ERR:wheel not found: ' + whl, file=sys.stderr)
    sys.exit(1)
with zipfile.ZipFile(whl) as zf:
    zf.extractall(sp)
print('OK')
WASMSH_PIP_EOF`,
        },
      });
      const exit = events.find((e) => "Exit" in e);
      if (exit && exit.Exit !== 0) {
        const stderr = decoder.decode(extractStream(events, "Stderr"));
        throw new Error(`Failed to extract wheel ${wheelPath}: ${stderr}`);
      }
    }

    async function downloadWheel(url) {
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`Failed to download ${url}: HTTP ${response.status}`);
      }
      const bytes = new Uint8Array(await response.arrayBuffer());
      const filename = url.split("/").pop() || "downloaded.whl";
      const fsPath = `/tmp/_wasmsh_pip_${filename}`;
      sendHostCommand({
        WriteFile: { path: fsPath, data: Array.from(bytes) },
      });
      return fsPath;
    }

    async function resolvePackageName(name) {
      const apiUrl = `https://pypi.org/pypi/${encodeURIComponent(name)}/json`;
      if (!isHostAllowed(apiUrl, sessionAllowedHosts)) {
        throw new Error(
          "Host 'pypi.org' not in allowedHosts. " +
          "Add 'pypi.org' and 'files.pythonhosted.org' to allowedHosts.",
        );
      }
      const response = await fetch(apiUrl);
      if (!response.ok) {
        if (response.status === 404) {
          throw new Error(`Package '${name}' not found on PyPI`);
        }
        throw new Error(`PyPI lookup failed for '${name}': HTTP ${response.status}`);
      }
      const data = await response.json();
      const urls = data.urls || [];
      const wheel = urls.find(
        (u) =>
          u.packagetype === "bdist_wheel" &&
          /-(py2\.py3|py3)-none-any\.whl$/i.test(u.filename),
      );
      if (!wheel) {
        throw new Error(
          `No pure-Python wheel found for '${name}'. ` +
          "Only pure-Python packages (py3-none-any) are supported.",
        );
      }
      if (!isHostAllowed(wheel.url, sessionAllowedHosts)) {
        throw new Error(
          `Download host not allowed for '${name}': ${wheel.url}. ` +
          "Add 'files.pythonhosted.org' to allowedHosts.",
        );
      }
      return { url: wheel.url, version: data.info?.version, filename: wheel.filename };
    }

    const installed = [];
    for (const req of reqs) {
      if (/^file:/i.test(req)) {
        throw new Error(`file: URIs are not supported for security: ${req}`);
      }

      if (req.startsWith("emfs:")) {
        extractWheel(req.slice(5));
        installed.push({ requirement: req });
      } else if (/^https?:\/\//i.test(req)) {
        if (!isHostAllowed(req, sessionAllowedHosts)) {
          throw new Error(
            `Host not allowed for package install: ${req}. ` +
            "Configure allowedHosts when creating the session.",
          );
        }
        const fsPath = await downloadWheel(req);
        extractWheel(fsPath);
        installed.push({ requirement: req });
      } else {
        if (sessionAllowedHosts.length === 0) {
          throw new Error(
            `Package name installs require network access: ${req}. ` +
            "Configure allowedHosts (e.g., ['pypi.org', 'files.pythonhosted.org']) when creating the session.",
          );
        }
        const resolved = await resolvePackageName(req);
        const fsPath = await downloadWheel(resolved.url);
        extractWheel(fsPath);
        installed.push({
          requirement: req,
          name: req,
          version: resolved.version,
        });
      }
    }
    return { installed, requirements: reqs };
  },

  async close() {
    if (moduleRef && runtimeHandle !== null) {
      moduleRef.ccall("wasmsh_runtime_free", null, ["number"], [runtimeHandle]);
    }
    runtimeHandle = null;
    moduleRef = null;
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
