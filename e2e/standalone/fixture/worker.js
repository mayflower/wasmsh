/**
 * Web Worker bootstrap for wasmsh.
 *
 * Accepts messages of the form:
 *   { type: "Init", step_budget: number }
 *   { type: "Run",  input: string }
 *   { type: "Cancel" }
 *   { type: "WriteFile", path: string, data: number[] }
 *   { type: "ReadFile",  path: string }
 *   { type: "ListDir",   path: string }
 *
 * Replies with { events: WorkerEvent[] } where events are the parsed
 * JSON array returned by WasmShell methods.
 */
import wasmInit, { WasmShell } from "./pkg/wasmsh_browser.js";

let shell = null;
let ready = false;
const pending = [];

async function boot() {
  await wasmInit();
  shell = new WasmShell();
  ready = true;
  // Drain any messages that arrived while loading.
  for (const msg of pending) {
    handle(msg);
  }
  pending.length = 0;
}

function handle(msg) {
  let json;
  switch (msg.type) {
    case "Init":
      json = shell.init(BigInt(msg.step_budget ?? 0));
      break;
    case "Run":
      json = shell.exec(msg.input);
      break;
    case "WriteFile":
      json = shell.write_file(msg.path, new Uint8Array(msg.data));
      break;
    case "ReadFile":
      json = shell.read_file(msg.path);
      break;
    case "ListDir":
      json = shell.list_dir(msg.path);
      break;
    case "Cancel":
      json = shell.cancel();
      break;
    default:
      self.postMessage({ error: "unknown command: " + msg.type });
      return;
  }
  self.postMessage({ events: JSON.parse(json) });
}

self.onmessage = function (e) {
  if (!ready) {
    pending.push(e.data);
  } else {
    handle(e.data);
  }
};

boot().catch(function (err) {
  self.postMessage({ error: err.message || String(err) });
});
