/**
 * Shared package install resolution logic for Node and browser paths.
 *
 * Handles bundled-package detection, allowlist enforcement, micropip
 * fallback, and pip subcommand dispatch (install, uninstall, list, freeze).
 * Platform-specific details (how to check if a wheel is locally
 * available) are injected via the `isBundled` callback.
 */
import { isHostAllowed } from "./allowlist.mjs";

/** Regex matching any pip invocation — used to intercept before the shell. */
const PIP_PREFIX_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)(?:\s+|$)/;

/** Regex matching pip install with arguments. */
const PIP_INSTALL_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+install\s+(.+)$/;

/** Regex matching pip uninstall with arguments. */
const PIP_UNINSTALL_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+uninstall\s+(.+)$/;

/** Regex matching pip list. */
const PIP_LIST_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+list\b/;

/** Regex matching pip freeze. */
const PIP_FREEZE_RE =
  /^\s*(?:pip3?|python3?\s+-m\s+pip)\s+freeze\b/;

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
      micropip = await ensureMicropip(pyodide);
    }
    await micropip.install(req, { deps: deps !== false });
    installed.push({ requirement: req });
  }

  return { installed, requirements: reqs };
}

/**
 * Try to handle a shell command as a pip invocation.
 *
 * Returns a RunResult if the command was a pip command (install, uninstall,
 * list, freeze, or unsupported subcommand).  Returns null if the command
 * is not a pip invocation at all.
 *
 * @param {string} command         Shell command string
 * @param {object} pyodide         The Pyodide API object
 * @param {(opts: {requirements: string[]}) => Promise<any>} installFn
 *   The host's installPythonPackages method (for install subcommand).
 * @returns {Promise<object|null>}  RunResult or null
 */
export async function handlePipCommand(command, pyodide, installFn) {
  // Not a pip command at all — let the shell handle it
  if (!PIP_PREFIX_RE.test(command)) return null;

  // pip install <packages>
  const installMatch = command.match(PIP_INSTALL_RE);
  if (installMatch) {
    const packages = installMatch[1]
      .split(/\s+/)
      .filter((a) => a && !a.startsWith("-"));
    if (packages.length === 0) {
      return shellResult("", "Usage: pip install <package> [package ...]\n", 1);
    }
    try {
      await installFn({ requirements: packages });
      const msg = packages.map((p) => `Successfully installed ${p}`).join("\n") + "\n";
      return shellResult(msg, "", 0);
    } catch (err) {
      return shellResult("", `ERROR: ${err.message}\n`, 1);
    }
  }

  // pip uninstall <packages>
  const uninstallMatch = command.match(PIP_UNINSTALL_RE);
  if (uninstallMatch) {
    const packages = uninstallMatch[1]
      .split(/\s+/)
      .filter((a) => a && !a.startsWith("-"));
    if (packages.length === 0) {
      return shellResult("", "Usage: pip uninstall <package> [package ...]\n", 1);
    }
    try {
      const micropip = await ensureMicropip(pyodide);
      micropip.uninstall(packages);
      const msg = packages.map((p) => `Successfully uninstalled ${p}`).join("\n") + "\n";
      return shellResult(msg, "", 0);
    } catch (err) {
      return shellResult("", `ERROR: ${err.message}\n`, 1);
    }
  }

  // pip list
  if (PIP_LIST_RE.test(command)) {
    try {
      const micropip = await ensureMicropip(pyodide);
      const pkgDict = micropip.list();
      const entries = [];
      for (const name of pkgDict.keys()) {
        const pkg = pkgDict.get(name);
        entries.push({ name, version: pkg.version, source: pkg.source });
      }
      pkgDict.destroy();
      entries.sort((a, b) => a.name.localeCompare(b.name));

      const nameW = Math.max(7, ...entries.map((e) => e.name.length));
      const verW = Math.max(7, ...entries.map((e) => e.version.length));
      let out = `${"Package".padEnd(nameW)} ${"Version".padEnd(verW)}\n`;
      out += `${"-".repeat(nameW)} ${"-".repeat(verW)}\n`;
      for (const e of entries) {
        out += `${e.name.padEnd(nameW)} ${e.version.padEnd(verW)}\n`;
      }
      return shellResult(out, "", 0);
    } catch (err) {
      return shellResult("", `ERROR: ${err.message}\n`, 1);
    }
  }

  // pip freeze
  if (PIP_FREEZE_RE.test(command)) {
    try {
      const micropip = await ensureMicropip(pyodide);
      const frozen = micropip.freeze();
      return shellResult(frozen + "\n", "", 0);
    } catch (err) {
      return shellResult("", `ERROR: ${err.message}\n`, 1);
    }
  }

  // pip (no args) or unsupported subcommand (pip show, pip search, etc.)
  const msg =
    "Usage: pip <command> [options]\n\n" +
    "Commands:\n" +
    "  install     Install packages\n" +
    "  uninstall   Uninstall packages\n" +
    "  list        List installed packages\n" +
    "  freeze      Output installed packages in lockfile format\n";
  return shellResult(msg, "", 0);
}

async function ensureMicropip(pyodide) {
  try {
    return pyodide.pyimport("micropip");
  } catch (error) {
    const missingModule = String(error?.message ?? error).includes("No module named 'micropip'");
    if (!missingModule) {
      throw error;
    }
  }

  await pyodide.loadPackage("micropip");
  return pyodide.pyimport("micropip");
}

function shellResult(stdout, stderr, exitCode) {
  return {
    events: [],
    stdout,
    stderr,
    output: stdout + stderr,
    exitCode,
  };
}
