const decoder = new TextDecoder();

let bootPromise = null;
let moduleRef = null;
let runtimeHandle = null;
let assetBaseUrl = null;

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
        let binary = "";
        for (const byte of respBytes) {
          binary += String.fromCharCode(byte);
        }
        const bodyBase64 = btoa(binary);

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

function extractStream(events, key) {
  const bytes = [];
  for (const event of events) {
    if (event && typeof event === "object" && key in event) {
      bytes.push(...event[key]);
    }
  }
  return new Uint8Array(bytes);
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
    const stdout = decoder.decode(extractStream(events, "Stdout"));
    const stderr = decoder.decode(extractStream(events, "Stderr"));
    return {
      events,
      stdout,
      stderr,
      output: stdout + stderr,
      exitCode: getExitCode(events),
    };
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
