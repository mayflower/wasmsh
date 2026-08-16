/**
 * Minimal `node:os` shim for the browser fixture.
 *
 * `fast-glob` — reached transitively through `deepagents` — calls
 * `os.platform()` at module load to decide whether to use Windows path
 * semantics. Vite's default `browser-external` stub throws on any property
 * access, so the whole bundle fails to initialise before the agent ever
 * starts. Only the handful of calls that actually occur are implemented;
 * anything else should fail loudly rather than return a plausible lie.
 */

/** Always POSIX: the sandbox VFS and every path in it use `/`. */
export function platform(): string {
  return "linux";
}

/** No real home directory exists in a browser tab. */
export function homedir(): string {
  return "/home/browser";
}

export function tmpdir(): string {
  return "/tmp";
}

export const EOL = "\n";

export default { platform, homedir, tmpdir, EOL };
