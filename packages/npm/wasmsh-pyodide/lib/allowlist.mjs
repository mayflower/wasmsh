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

  const host = parsed.hostname.toLowerCase();
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
      if (host === suffix || host.endsWith(`.${suffix}`)) {
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
