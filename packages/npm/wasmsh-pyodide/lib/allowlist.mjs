/**
 * Check if a URL's host is in the allowlist.
 * Mirrors the Rust HostAllowlist semantics: exact host, wildcard subdomain
 * (*.example.com), and host:port. Empty list denies all.
 */
export function isHostAllowed(url, allowedHosts) {
  if (!allowedHosts || allowedHosts.length === 0) {
    return false;
  }

  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }

  // Scheme allowlist parity with the Rust HostAllowlist (audit F9).
  // file:, javascript:, data:, ws:, wss:, ftp:, blob: are rejected before
  // the host lookup so policy stays identical across the three
  // implementations (Rust, JS membrane, JS allowlist).
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return false;
  }

  // Strip the FQDN trailing dot so `example.com.` matches `example.com`,
  // matching the Rust implementation.
  let host = parsed.hostname.toLowerCase();
  if (host.endsWith(".")) host = host.slice(0, -1);
  const port = parsed.port ? Number(parsed.port) : null;

  for (const pattern of allowedHosts) {
    const colonIdx = pattern.lastIndexOf(":");
    let patHost;
    let patPort;
    if (colonIdx > 0 && /^\d+$/.test(pattern.slice(colonIdx + 1))) {
      patHost = pattern.slice(0, colonIdx).toLowerCase();
      patPort = Number(pattern.slice(colonIdx + 1));
    } else {
      patHost = pattern.toLowerCase();
      patPort = null;
    }

    if (patPort !== null && port !== patPort) {
      continue;
    }

    if (patHost.startsWith("*.")) {
      const suffix = patHost.slice(2);
      // `*.example.com` matches strict subdomains only; the apex `example.com`
      // is NOT covered. Callers wanting the apex must list it explicitly.
      // Mirrors crates/wasmsh-utils/src/net_types.rs.
      if (host.endsWith(`.${suffix}`)) {
        return true;
      }
      continue;
    }

    if (host === patHost) {
      return true;
    }
  }

  return false;
}
