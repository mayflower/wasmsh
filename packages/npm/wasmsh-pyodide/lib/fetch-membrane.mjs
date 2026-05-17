/**
 * Install a `globalThis.fetch` wrapper that brokers every fetch through
 * the host allowlist, manual redirect handling, and a streamed response
 * cap.
 *
 * This is the Pyodide membrane (audit C1/C2 + F2/F3): inside a session
 * worker, Pyodide exposes the host's `fetch` to Python via the `js`
 * proxy. Without this wrapper, Python code (direct `js.fetch`,
 * `pyodide.http.pyfetch`, or `micropip`) would reach the network
 * bypassing the same controls that `curl` is forced through.
 *
 * The wrapper:
 * - Validates the URL against `allowedHosts` via `isHostAllowed()`.
 *   Allowlist semantics MUST match the Rust `HostAllowlist` and the
 *   `lib/allowlist.mjs` helper.
 * - Forces `redirect: "manual"` on the underlying fetch and drives the
 *   chain in JS so every hop's Location is re-validated. The audit
 *   (F3) called this out — without per-hop revalidation a server on an
 *   allowed host could 302 the broker to a forbidden one
 *   (cloud-metadata, link-local, private RFC1918, etc.).
 * - Pre-checks `Content-Length` against the response cap, then streams
 *   the body and aborts after the cap is exceeded. Either guard alone
 *   is insufficient: servers can omit Content-Length, and servers can
 *   lie about it.
 * - Applies a default wall-clock timeout via AbortController.
 *
 * State that the audit (F2) called out — the captured original fetch
 * and the allow-list — lives inside the closure of the wrapper. Python
 * cannot reach it through the `js` proxy.
 *
 * Re-arming: a worker that swaps sessions can call this again with a
 * fresh `allowedHosts` list; the wrapper is replaced and the previous
 * closure is dropped. Subsequent fetches see the new list immediately.
 */

import { isHostAllowed } from "./allowlist.mjs";

const MEMBRANE_FLAG = Symbol.for("wasmsh.fetch-membrane");

const DEFAULT_TIMEOUT_MS = 30_000;
const DEFAULT_MAX_RESPONSE_BYTES = 64 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS = 20;

function readinessUrl(input) {
  if (typeof input === "string") return input;
  if (input && typeof input.url === "string") return input.url;
  try {
    return String(input);
  } catch {
    return "";
  }
}

function methodOf(input, init) {
  if (init && typeof init.method === "string") return init.method.toUpperCase();
  if (input && typeof input.method === "string") return input.method.toUpperCase();
  return "GET";
}

class HostDeniedError extends TypeError {
  constructor(url) {
    super(`wasmsh: host denied by sandbox allowlist: ${url}`);
    this.name = "WasmshHostDenied";
    this.code = "WASMSH_HOST_DENIED";
  }
}

class ResponseTooLargeError extends TypeError {
  constructor(received, cap) {
    super(`wasmsh: response exceeds max_response_bytes (${received} > ${cap})`);
    this.name = "WasmshResponseTooLarge";
    this.code = "WASMSH_RESPONSE_TOO_LARGE";
  }
}

class TooManyRedirectsError extends TypeError {
  constructor(limit) {
    super(`wasmsh: exceeded ${limit} redirects`);
    this.name = "WasmshTooManyRedirects";
    this.code = "WASMSH_TOO_MANY_REDIRECTS";
  }
}

function shouldDowngradeToGet(status) {
  return status === 301 || status === 302 || status === 303;
}

function isRedirect(status) {
  return status === 301 || status === 302 || status === 303 ||
    status === 307 || status === 308;
}

/**
 * Wrap a Response so reading the body enforces `maxBytes`. We re-stream
 * the original body through a TransformStream that counts as it goes,
 * cancelling the source as soon as the cap is exceeded. Both
 * `arrayBuffer()` and `text()` go through `.body` so the cap applies to
 * every read path Python code uses, including `pyodide.http.pyfetch`'s
 * own `bytes()`/`.string()` helpers.
 */
function capResponse(response, maxBytes) {
  if (!response.body) {
    return response;
  }
  const contentLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > maxBytes) {
    // Cancel the body and fail before draining the socket. Use a fresh
    // empty Response so the caller can still inspect headers/status.
    try { response.body.cancel(); } catch { /* already locked / cancelled */ }
    throw new ResponseTooLargeError(contentLength, maxBytes);
  }
  let received = 0;
  const source = response.body;
  const capped = new ReadableStream({
    async start(controller) {
      const reader = source.getReader();
      try {
        while (true) {
          const { value, done } = await reader.read();
          if (done) {
            controller.close();
            return;
          }
          if (value) {
            received += value.byteLength;
            if (received > maxBytes) {
              controller.error(new ResponseTooLargeError(received, maxBytes));
              try { await reader.cancel(); } catch { /* swallow */ }
              return;
            }
            controller.enqueue(value);
          }
        }
      } catch (err) {
        controller.error(err);
      }
    },
    cancel(reason) {
      try { source.cancel(reason); } catch { /* swallow */ }
    },
  });
  return new Response(capped, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}

/** Resolve a redirect Location header against the previous request URL. */
function resolveRedirect(prevUrl, location) {
  try {
    return new URL(location, prevUrl).toString();
  } catch {
    return null;
  }
}

export function installFetchMembrane(globalObject, allowedHosts) {
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

  const state = {
    allowedHosts: hosts,
    maxResponseBytes: DEFAULT_MAX_RESPONSE_BYTES,
    maxRedirects: DEFAULT_MAX_REDIRECTS,
    timeoutMs: DEFAULT_TIMEOUT_MS,
  };

  async function brokeredFetch(input, init) {
    let url = readinessUrl(input);
    if (!isHostAllowed(url, state.allowedHosts)) {
      throw new HostDeniedError(url);
    }

    // Drive the redirect chain ourselves so every hop is re-validated.
    // Force redirect:"manual" on the underlying transport regardless of
    // what the caller asked for.
    const callerInit = init || {};
    let method = methodOf(input, init);
    let body = callerInit.body;

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), state.timeoutMs);

    try {
      let hopUrl = url;
      let hops = 0;
      while (true) {
        const hopInit = {
          ...callerInit,
          method,
          body,
          redirect: "manual",
          signal: controller.signal,
        };
        // Pass the resolved URL string on subsequent hops so any Request
        // body/headers from the caller are still honored on the first hop.
        const target = hops === 0 ? input : hopUrl;
        const response = await originalFetch(target, hopInit);
        if (!isRedirect(response.status)) {
          return capResponse(response, state.maxResponseBytes);
        }
        const location = response.headers.get("location");
        if (!location) {
          return capResponse(response, state.maxResponseBytes);
        }
        if (hops >= state.maxRedirects) {
          try { response.body?.cancel(); } catch { /* swallow */ }
          throw new TooManyRedirectsError(state.maxRedirects);
        }
        const next = resolveRedirect(hopUrl, location);
        if (!next) {
          try { response.body?.cancel(); } catch { /* swallow */ }
          throw new TypeError(`wasmsh: invalid redirect target: ${location}`);
        }
        if (!isHostAllowed(next, state.allowedHosts)) {
          try { response.body?.cancel(); } catch { /* swallow */ }
          throw new HostDeniedError(next);
        }
        // RFC 7231: 301/302/303 demote non-GET/HEAD to GET and drop body.
        if (shouldDowngradeToGet(response.status) && method !== "GET" && method !== "HEAD") {
          method = "GET";
          body = undefined;
        }
        hopUrl = next;
        hops += 1;
        // Drain headers so the socket can be reused for the next hop.
        try { response.body?.cancel(); } catch { /* swallow */ }
      }
    } finally {
      clearTimeout(timer);
    }
  }

  globalObject.fetch = brokeredFetch;
  // `configurable: true` lets tests re-install the membrane against a
  // different transport. Python cannot reach Symbol-keyed properties
  // through the `js` proxy, so user code in Pyodide cannot delete the
  // flag to bypass the wrapper.
  Object.defineProperty(globalObject, MEMBRANE_FLAG, {
    value: state,
    writable: false,
    enumerable: false,
    configurable: true,
  });
}
