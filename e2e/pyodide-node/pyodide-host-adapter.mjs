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
import { createRuntimeBridge } from "../../packages/npm/wasmsh-pyodide/lib/runtime-bridge.mjs";

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
  const runtimeBridge = createRuntimeBridge(mod);

  // Init immediately
  const initEvents = runtimeBridge.sendHostCommand({ Init: { step_budget: stepBudget } });

  return {
    /** The events returned by Init (includes Version). */
    initEvents,

    /** Send a HostCommand and return the WorkerEvent array. */
    send(cmd) {
      return Promise.resolve(runtimeBridge.sendHostCommand(cmd));
    },

    /** Free the underlying runtime. */
    destroy() {
      runtimeBridge.close();
    },
  };
}
