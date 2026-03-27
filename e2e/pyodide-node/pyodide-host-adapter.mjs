/**
 * Thin host adapter for the wasmsh Pyodide runtime.
 *
 * Wraps the raw C ABI (wasmsh_runtime_new / handle_json / free) into a
 * clean send(command) → events[] interface that mirrors the standalone
 * worker protocol.
 *
 * Usage:
 *   const adapter = await createHostAdapter({ stepBudget: 0 });
 *   const events = await adapter.send({ Run: { input: "echo hi" } });
 *   adapter.destroy();
 */
import { createProbeModule, createFullModule } from "./host-wrapper.mjs";

/**
 * Create a host adapter backed by a wasmsh runtime inside Pyodide.
 *
 * @param {object} [opts]
 * @param {number} [opts.stepBudget=0] — step budget for Init (0 = unlimited)
 * @param {boolean} [opts.fullPython=false] — boot CPython (needed for python3 command)
 * @returns {{ send(cmd): Promise<object[]>, destroy(): void }}
 */
export async function createHostAdapter(opts = {}) {
  const stepBudget = opts.stepBudget ?? 0;
  const mod = opts.fullPython
    ? await createFullModule()
    : await createProbeModule();

  const handle = mod.ccall("wasmsh_runtime_new", "number", [], []);
  if (handle === 0) throw new Error("wasmsh_runtime_new returned null");

  // Init immediately
  const initEvents = sendRaw({ Init: { step_budget: stepBudget } });

  function sendRaw(cmd) {
    const json = typeof cmd === "string" ? JSON.stringify(cmd) : JSON.stringify(cmd);
    const jsonPtr = mod.stringToNewUTF8(json);
    const resultPtr = mod.ccall(
      "wasmsh_runtime_handle_json",
      "number",
      ["number", "number"],
      [handle, jsonPtr],
    );
    mod._free(jsonPtr);
    const resultStr = mod.UTF8ToString(resultPtr);
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [resultPtr]);
    return JSON.parse(resultStr);
  }

  return {
    /** The events returned by Init (includes Version). */
    initEvents,

    /** Send a HostCommand and return the WorkerEvent array. */
    send(cmd) {
      return Promise.resolve(sendRaw(cmd));
    },

    /** Free the underlying runtime. */
    destroy() {
      mod.ccall("wasmsh_runtime_free", null, ["number"], [handle]);
    },
  };
}
