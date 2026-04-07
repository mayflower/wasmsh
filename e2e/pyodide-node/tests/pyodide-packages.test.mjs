/**
 * Smoke tests for popular Pyodide packages installed via micropip.
 *
 * These use the real micropip (pre-installed via loadPyodide) to install
 * packages from the Pyodide CDN or PyPI into the sandbox.
 *
 * Both pure-Python and C extension packages are expected to import
 * successfully now that the wasm is built with MAIN_MODULE=1 (compiled
 * side modules can resolve CPython / libc / libstdc++ symbols at runtime).
 *
 * Skip: SKIP_PYODIDE=1 or SKIP_NETWORK=1
 */
import { describe, it } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { createSessionTracker } from "./test-session-helper.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");

const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  process.env.SKIP_NETWORK === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

const ALLOWED_HOSTS = [
  "cdn.jsdelivr.net",
  "pypi.org",
  "files.pythonhosted.org",
];

describe("Pyodide packages", () => {
  const openSession = SKIP ? null : createSessionTracker(createNodeSession, ASSETS_DIR);
  const openNetworkSession = SKIP ? null : async () => openSession({ allowedHosts: ALLOWED_HOSTS });

  // ── Pure-Python packages (always work) ───────────────────────

  for (const [pkg, importCheck] of [
    ["micropip", "import micropip; print(micropip.__version__)"],
    ["six", "import six; print(six.__version__)"],
    ["attrs", "import attrs; print(attrs.__version__)"],
    ["click", "import click; print(click.__version__)"],
    ["packaging", "import packaging; print(packaging.__version__)"],
    ["beautifulsoup4", "import bs4; print(bs4.__version__)"],
    ["networkx", "import networkx; print(networkx.__version__)"],
    ["idna", "import idna; print(idna.__version__)"],
    ["certifi", "import certifi; print(certifi.__version__)"],
  ]) {
    it(pkg, { skip: SKIP, timeout: 120_000 }, async () => {
      const s = await openNetworkSession();
      await s.installPythonPackages(pkg);
      const r = await s.run(`python3 -c "${importCheck}"`);
      assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
      assert.ok(r.stdout.trim().length > 0, "should print version");
    });
  }

  // ── C extension with pure-Python fallback ────────────────────

  it("pyyaml", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openNetworkSession();
    await s.installPythonPackages("pyyaml");
    const r = await s.run('python3 -c "import yaml; print(yaml.__version__)"');
    assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
  });

  // ── Compiled C extension packages (load .so side modules) ────
  // These ship .cpython-*-pyodide_*-wasm32.so binaries that resolve
  // CPython / libc / libstdc++ symbols at runtime against the main
  // module. They only work because the wasm is built with MAIN_MODULE=1.

  for (const [pkg, importMod] of [
    ["jsonschema", "jsonschema"],
    ["pydantic", "pydantic"],
    ["regex", "regex"],
    ["numpy", "numpy"],
    ["pandas", "pandas"],
    ["scipy", "scipy"],
  ]) {
    it(`${pkg} — install + import compiled side module`, { skip: SKIP, timeout: 180_000 }, async () => {
      const s = await openNetworkSession();
      const result = await s.installPythonPackages(pkg);
      assert.ok(result.installed.length > 0, "install should succeed");
      const r = await s.run(`python3 -c "import ${importMod}; print('ok')"`);
      assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
      assert.ok(r.stdout.includes("ok"), `unexpected stdout: ${r.stdout}`);
    });
  }
});
