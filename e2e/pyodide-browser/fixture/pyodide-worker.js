/**
 * Browser Web Worker for the custom Pyodide + wasmsh runtime.
 *
 * Boots the Emscripten module, inits CPython, creates a wasmsh runtime,
 * then handles messages in the same protocol as the standalone worker.
 */

// pyodide.asm.js sets globalThis._createPyodideModule
try {
  importScripts("./dist/pyodide.asm.js");
} catch (e) {
  self.postMessage({ error: "importScripts failed: " + e.message });
}

const SENTINEL_MARKER = {};
const sentinelStubs = {
  create_sentinel: () => SENTINEL_MARKER,
  is_sentinel: (v) => v === SENTINEL_MARKER ? 1 : 0,
};

let Module = null;
let runtimeHandle = null;
const pendingMessages = [];

// Pre-fetch the stdlib zip as proper ArrayBuffer before starting the factory.
var preloadedStdlib = null;

async function boot() {
  try {
    var resp = await fetch("./dist/python_stdlib.zip");
    if (resp.ok) {
      preloadedStdlib = new Uint8Array(await resp.arrayBuffer());
    }
  } catch (e) {
    // no stdlib — Python won't work but shell still will
  }
  const factory = self._createPyodideModule;
  if (typeof factory !== "function") {
    throw new Error("_createPyodideModule not found, got: " + typeof factory);
  }

  const api = {
    tests: [],
    config: { jsglobals: self, indexURL: "./dist/" },
    fatal_error: function () {},
    on_fatal: function () {},
    initializeStreams: function () {},
    finalizeBootstrap: function () {},
    version: "0.0.0",
    lockfile_info: {},
    loadBinaryFile: function (path) {
      var xhr = new XMLHttpRequest();
      xhr.open("GET", "./dist/" + path, false);
      // arraybuffer not allowed on sync XHR — use binary string instead.
      xhr.overrideMimeType("text/plain; charset=x-user-defined");
      xhr.send();
      if (xhr.status !== 200) return new Uint8Array(0);
      var text = xhr.responseText;
      var bytes = new Uint8Array(text.length);
      for (var i = 0; i < text.length; i++) {
        bytes[i] = text.charCodeAt(i) & 0xff;
      }
      return bytes;
    },
    runtimeEnv: {
      IN_NODE: false,
      IN_BROWSER: true,
      IN_BROWSER_MAIN_THREAD: false,
      IN_BROWSER_WEB_WORKER: true,
    },
  };

  // Resolve the Module via onRuntimeInitialized.
  Module = await new Promise(function (resolve, reject) {
    factory({
      noInitialRun: true,
      thisProgram: "wasmsh-pyodide",
      locateFile: function (path) { return "./dist/" + path; },
      print: function (t) { self._pyLogs = (self._pyLogs || []).concat(t); },
      printErr: function (t) { self._pyErrs = (self._pyErrs || []).concat(t); },
      API: api,

      instantiateWasm: function (imports, successCallback) {
        imports.sentinel = sentinelStubs;
        // Provide network fetch in env namespace for curl/wget.
        if (!imports.env) imports.env = {};
        imports.env.wasmsh_js_http_fetch = function (urlPtr, methodPtr, headersJsonPtr, bodyPtr, bodyLen, followRedirects) {
          if (!Module) return 0;
          var url = Module.UTF8ToString(urlPtr);
          var method = Module.UTF8ToString(methodPtr);
          var headersJson = Module.UTF8ToString(headersJsonPtr);
          var bodyBytes = null;
          if (bodyPtr !== 0 && bodyLen > 0) {
            bodyBytes = new Uint8Array(Module.HEAPU8.buffer, bodyPtr, bodyLen).slice();
          }
          var result;
          try {
            var xhr = new XMLHttpRequest();
            xhr.open(method, url, false);
            var headers = JSON.parse(headersJson || "[]");
            for (var h = 0; h < headers.length; h++) {
              xhr.setRequestHeader(headers[h][0], headers[h][1]);
            }
            xhr.responseType = "arraybuffer";
            xhr.send(bodyBytes);
            var respBytes = new Uint8Array(xhr.response || new ArrayBuffer(0));
            var binary = "";
            for (var b = 0; b < respBytes.length; b++) binary += String.fromCharCode(respBytes[b]);
            var respHeaders = xhr.getAllResponseHeaders().split("\r\n").filter(function(h){return h;}).map(function(h){
              var idx = h.indexOf(": ");
              return idx >= 0 ? [h.slice(0, idx), h.slice(idx + 2)] : [h, ""];
            });
            result = JSON.stringify({ status: xhr.status, headers: respHeaders, body_base64: btoa(binary) });
          } catch (e) {
            result = JSON.stringify({ status: 0, headers: [], body_base64: "", error: e.message });
          }
          return Module.stringToNewUTF8(result);
        };
        fetch("./dist/pyodide.asm.wasm")
          .then(function (resp) {
            return resp.arrayBuffer();
          })
          .then(function (buf) {
            var bytes = new Uint8Array(buf);
            return WebAssembly.instantiate(buf, imports).then(function (result) {
              successCallback(result.instance, bytes);
            });
          })
          .catch(function () {});
        return {};
      },

      preRun: [function (m) {
        // Mount pre-fetched stdlib zip.
        if (preloadedStdlib && preloadedStdlib.length > 0) {
          m.FS.mkdirTree("/lib/python3.13");
          m.FS.writeFile("/lib/python3.13/python_stdlib.zip", preloadedStdlib);
          m.ENV.PYTHONPATH = "/lib/python3.13/python_stdlib.zip";
          m.ENV.PYTHONHOME = "/";
        }
        m.FS.mkdirTree("/workspace");
      }],

      onRuntimeInitialized: function () { resolve(this); },
    }).catch(function () {});
  });

  // Boot CPython.
  Module.callMain([]);

  // Create wasmsh runtime and init.
  runtimeHandle = Module.ccall("wasmsh_runtime_new", "number", [], []);
  var initJson = JSON.stringify({ Init: { step_budget: 0 } });
  var initPtr = Module.stringToNewUTF8(initJson);
  var initResPtr = Module.ccall("wasmsh_runtime_handle_json", "number",
    ["number", "number"], [runtimeHandle, initPtr]);
  Module._free(initPtr);
  Module.ccall("wasmsh_runtime_free_string", null, ["number"], [initResPtr]);

  // Signal ready.
  self.postMessage({ type: "ready" });

  // Drain pending messages.
  for (var i = 0; i < pendingMessages.length; i++) {
    handleMessage(pendingMessages[i]);
  }
  pendingMessages.length = 0;
}

function handleMessage(msg) {
  // Build the HostCommand JSON from the message.
  var cmd;
  switch (msg.type) {
    case "Init":
      cmd = { Init: { step_budget: msg.step_budget || 0, allowed_hosts: msg.allowed_hosts || [] } };
      break;
    case "Run":
      cmd = { Run: { input: msg.input } };
      break;
    case "WriteFile":
      cmd = { WriteFile: { path: msg.path, data: msg.data } };
      break;
    case "ReadFile":
      cmd = { ReadFile: { path: msg.path } };
      break;
    case "ListDir":
      cmd = { ListDir: { path: msg.path } };
      break;
    case "Cancel":
      cmd = "Cancel";
      break;
    default:
      self.postMessage({ error: "unknown command: " + msg.type });
      return;
  }

  var json = JSON.stringify(cmd);
  var jsonPtr = Module.stringToNewUTF8(json);
  var resultPtr = Module.ccall("wasmsh_runtime_handle_json", "number",
    ["number", "number"], [runtimeHandle, jsonPtr]);
  Module._free(jsonPtr);
  var resultStr = Module.UTF8ToString(resultPtr);
  Module.ccall("wasmsh_runtime_free_string", null, ["number"], [resultPtr]);
  self.postMessage({ events: JSON.parse(resultStr) });
}

self.onmessage = function (e) {
  if (Module && runtimeHandle) {
    handleMessage(e.data);
  } else {
    pendingMessages.push(e.data);
  }
};

boot().then(function () {
  // boot completed successfully
}).catch(function (err) {
  self.postMessage({ error: "boot failed: " + (err.message || String(err)) });
});
