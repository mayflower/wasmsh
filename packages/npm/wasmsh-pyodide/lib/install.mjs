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
  const debug = installDebugSink(pyodide);

  for (const req of reqs) {
    if (/^file:/i.test(req)) {
      throw new Error(`file: URIs are not supported for security: ${req}`);
    }

    const isPlainName =
      !req.startsWith("emfs:") && !/^https?:\/\//i.test(req);

    // Bundled packages: resolve offline via pyodide.loadPackage()
    if (isPlainName && (await isBundled(req))) {
      const loaderErrors = [];
      try {
        await pyodide.loadPackage(req, {
          messageCallback: (message) => debug?.log(`loadPackage: ${message}`),
          errorCallback: (message) => {
            loaderErrors.push(String(message));
            debug?.log(`loadPackage error: ${message}`);
          },
        });
      } catch (err) {
        throw new Error(
          `Failed to load bundled package '${req}' from local assets: ${err.message}. ` +
            "This may indicate a corrupt wheel file or missing symbol exports in the build.",
        );
      }
      assertPackageLoaded(pyodide, req, loaderErrors);
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
    debug?.log(`micropip.install(${JSON.stringify(req)})`);
    debug?.begin();
    try {
      await micropip.install(req, { deps: deps !== false, verbose: debug !== null });
    } finally {
      debug?.end();
    }
    if (isPlainName) {
      assertPackageLoaded(pyodide, req, []);
    }
    installed.push({ requirement: req });
  }

  return { installed, requirements: reqs };
}

/**
 * Opt-in install diagnostics (`WASMSH_PIP_DEBUG=1`).
 *
 * The host discards the interpreter's stdout/stderr because shell output
 * travels over the protocol instead, which also hides everything micropip
 * and `loadPackage` say about a resolution. With the flag set, those
 * messages go to the host's stderr while an install is in flight.
 */
function installDebugSink(pyodide) {
  if (!globalThis.process?.env?.WASMSH_PIP_DEBUG) {
    return null;
  }
  const log = (line) => process.stderr.write(`[wasmsh pip] ${line}\n`);
  const python = (line) => log(`py: ${line}`);
  const silent = () => {};
  return {
    log,
    // The host boots Pyodide with no-op stdio handlers; route the
    // interpreter's output here only while micropip runs.
    begin() {
      pyodide.setStdout?.({ batched: python });
      pyodide.setStderr?.({ batched: python });
    },
    end() {
      pyodide.setStdout?.({ batched: silent });
      pyodide.setStderr?.({ batched: silent });
    },
  };
}

/** Project name of a plain requirement (`numpy>=2`, `pkg[extra]`), PEP 503-normalized. */
function normalizedPackageName(requirement) {
  const match = /^[A-Za-z0-9][A-Za-z0-9._-]*/.exec(requirement.trim());
  return (match ? match[0] : requirement).toLowerCase().replace(/[-_.]+/g, "-");
}

/**
 * Fail loudly when an install did not actually make the package importable.
 *
 * Pyodide's `loadPackage` reports a wheel whose side module failed to link
 * (or whose download failed) through `errorCallback` and resolves anyway,
 * and micropip inherits that behaviour for lockfile packages. Without this
 * check such an install returns success and the caller only finds out at
 * import time, with a `ModuleNotFoundError` that names nothing useful.
 */
function assertPackageLoaded(pyodide, requirement, loaderErrors) {
  const wanted = normalizedPackageName(requirement);
  const loaded = Object.keys(pyodide.loadedPackages ?? {});
  if (loaded.some((name) => normalizedPackageName(name) === wanted)) {
    return;
  }
  const detail = loaderErrors.length
    ? ` Loader reported: ${loaderErrors.join(" | ")}`
    : "";
  throw new Error(
    `Package '${requirement}' was not loaded after install.${detail} ` +
      `Loaded packages: ${loaded.length ? loaded.join(", ") : "<none>"}`,
  );
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
