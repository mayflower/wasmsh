import { isHostAllowed } from "../../../packages/npm/wasmsh-pyodide/lib/allowlist.mjs";

export function assertAllowedHost(url, allowedHosts) {
  if (!isHostAllowed(url, allowedHosts)) {
    throw new Error(`host denied: ${url}`);
  }
}
