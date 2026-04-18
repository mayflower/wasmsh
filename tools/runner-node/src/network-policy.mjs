import { isHostAllowed } from "../../../packages/npm/wasmsh-pyodide/lib/allowlist.mjs";

/**
 * Thrown by the fetch broker when an allowlist check rejects a URL.
 * Callers `instanceof`-check this class rather than matching on
 * `error.message`, so future message tweaks cannot accidentally
 * promote a denied-host error into a generic transport failure.
 */
export class HostDeniedError extends Error {
  constructor(url) {
    super(`host denied: ${url}`);
    this.name = "HostDeniedError";
    this.url = url;
  }
}

export function assertAllowedHost(url, allowedHosts) {
  if (!isHostAllowed(url, allowedHosts)) {
    throw new HostDeniedError(url);
  }
}
