/**
 * Install a `globalThis.fetch` wrapper that enforces the host allowlist.
 *
 * This is part of the Pyodide "membrane" (audit C1/C2): inside a session
 * worker, Pyodide exposes Node's `globalThis.fetch` to Python via the `js`
 * proxy. Without this wrapper, Python code (direct `js.fetch`,
 * `pyodide.http.pyfetch`, or `micropip`) would reach the network bypassing
 * the same allowlist that `curl` is forced through.
 *
 * The wrapper is intentionally minimal and idempotent: calling it twice with
 * the same `allowedHosts` is a no-op the second time. Re-running with a
 * new list updates the cached set so a single worker that handles two
 * sequential sessions cannot leak the previous allowlist.
 *
 * Allowlist semantics MUST match the Rust `HostAllowlist` and the
 * `lib/allowlist.mjs` helper — see those files for the canonical wildcard
 * apex-exclusion behavior.
 */

import { isHostAllowed } from "./allowlist.mjs";

const MEMBRANE_FLAG = Symbol.for("wasmsh.fetch-membrane");

export function installFetchMembrane(globalObject, allowedHosts) {
  // Re-bind on every call so a worker that swaps sessions picks up the
  // newest list. The wrapper closes over `allowedHosts` directly.
  const hosts = Array.isArray(allowedHosts) ? allowedHosts.slice() : [];
  const existing = globalObject[MEMBRANE_FLAG];
  if (existing) {
    existing.allowedHosts = hosts;
    return;
  }

  const originalFetch = globalObject.fetch?.bind(globalObject);
  if (typeof originalFetch !== "function") {
    // No fetch on this global (older runtime, test stub). Nothing to wrap.
    return;
  }

  const state = { allowedHosts: hosts };

  async function brokeredFetch(input, init) {
    const url = typeof input === "string"
      ? input
      : (input && (input.url || String(input))) || "";
    if (!isHostAllowed(url, state.allowedHosts)) {
      const err = new TypeError(
        `wasmsh: host denied by sandbox allowlist: ${url}`,
      );
      err.code = "WASMSH_HOST_DENIED";
      throw err;
    }
    return originalFetch(input, init);
  }

  globalObject.fetch = brokeredFetch;
  Object.defineProperty(globalObject, MEMBRANE_FLAG, {
    value: state,
    writable: false,
    enumerable: false,
    configurable: false,
  });
}
