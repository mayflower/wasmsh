/**
 * Shared package install resolution logic for Node and browser paths.
 *
 * Handles bundled-package detection, allowlist enforcement, and micropip
 * fallback.  Platform-specific details (how to check if a wheel is locally
 * available) are injected via the `isBundled` callback.
 */
import { isHostAllowed } from "./allowlist.mjs";

/** Regex matching pip install commands intercepted by the shell layer. */
export const PIP_INSTALL_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+install\s+(.+)$/;

/**
 * Install one or more Python packages.
 *
 * @param {string[]} reqs          Package requirements (names, URLs, emfs: paths)
 * @param {object}   pyodide       The Pyodide API object
 * @param {object}   opts
 * @param {(name: string) => boolean | Promise<boolean>} opts.isBundled
 *   Returns true if the package has a locally available wheel.
 * @param {string[]} opts.allowedHosts  Hosts allowed for network installs.
 * @param {boolean}  [opts.deps=true]   Install dependencies.
 * @returns {Promise<{installed: Array<{requirement: string}>, requirements: string[]}>}
 */
export async function installPackages(reqs, pyodide, opts) {
  const { isBundled, allowedHosts, deps = true } = opts;
  const installed = [];
  let micropip = null;

  for (const req of reqs) {
    if (/^file:/i.test(req)) {
      throw new Error(`file: URIs are not supported for security: ${req}`);
    }

    const isPlainName =
      !req.startsWith("emfs:") && !/^https?:\/\//i.test(req);

    // Bundled packages: resolve offline via pyodide.loadPackage()
    if (isPlainName && (await isBundled(req))) {
      try {
        await pyodide.loadPackage(req);
      } catch (err) {
        throw new Error(
          `Failed to load bundled package '${req}' from local assets: ${err.message}. ` +
            "This may indicate a corrupt wheel file or missing symbol exports in the build.",
        );
      }
      installed.push({ requirement: req });
      continue;
    }

    if (
      /^https?:\/\//i.test(req) &&
      !isHostAllowed(req, allowedHosts)
    ) {
      throw new Error(
        `Host not allowed for package install: ${req}. ` +
          "Configure allowedHosts when creating the session.",
      );
    }
    if (isPlainName && allowedHosts.length === 0) {
      throw new Error(
        `Package name installs require network access: ${req}. ` +
          "Configure allowedHosts (e.g., ['cdn.jsdelivr.net', 'pypi.org', 'files.pythonhosted.org']) when creating the session.",
      );
    }

    if (!micropip) {
      micropip = pyodide.pyimport("micropip");
    }
    await micropip.install(req, { deps: deps !== false });
    installed.push({ requirement: req });
  }

  return { installed, requirements: reqs };
}

/**
 * Parse a pip install command string into package names.
 * Returns null if the command is not a pip install.
 */
export function parsePipInstall(command) {
  const m = command.match(PIP_INSTALL_RE);
  if (!m) return null;
  return m[1]
    .split(/\s+/)
    .filter((a) => a && !a.startsWith("-"));
}

/**
 * Format the result of a pip install for shell output.
 */
export function formatPipResult(packages) {
  const msg =
    packages.map((p) => `Successfully installed ${p}`).join("\n") + "\n";
  return { events: [], stdout: msg, stderr: "", output: msg, exitCode: 0 };
}

/**
 * Format a pip install error for shell output.
 */
export function formatPipError(err) {
  const msg = `ERROR: ${err.message}\n`;
  return { events: [], stdout: "", stderr: msg, output: msg, exitCode: 1 };
}

export const PIP_USAGE_ERROR = {
  events: [],
  stdout: "",
  stderr: "Usage: pip install <package> [package ...]\n",
  output: "Usage: pip install <package> [package ...]\n",
  exitCode: 1,
};
