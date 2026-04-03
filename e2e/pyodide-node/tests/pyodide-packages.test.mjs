/**
 * Smoke tests for popular Pyodide packages installed via micropip.
 *
 * These use the real micropip (pre-installed via loadPyodide) to install
 * packages from the Pyodide CDN or PyPI into the sandbox.
 *
 * C extension packages require MAIN_MODULE=1 for dlopen. Those that fail
 * to import document the current limitation and will pass once the build
 * switches to MAIN_MODULE=1 + EXPORT_ALL=0.
 *
 * Skip: SKIP_PYODIDE=1 or SKIP_NETWORK=1
 */
import { describe, it, after } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

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
  const sessions = [];
  after(async () => {
    for (const s of sessions) {
      try { await s.close(); } catch { /* ignore */ }
    }
  });

  async function openSession() {
    const session = await createNodeSession({
      assetDir: ASSETS_DIR,
      allowedHosts: ALLOWED_HOSTS,
    });
    sessions.push(session);
    return session;
  }

  // ── Pure-Python packages (always work) ───────────────────────

  for (const [pkg, importCheck] of [
    ["micropip", "import micropip; print(micropip.__version__)"],
    ["six", "import six; print(six.__version__)"],
    ["attrs", "import attrs; print(attrs.__version__)"],
    ["click", "import click; print(click.__version__)"],
    ["packaging", "import packaging; print(packaging.__version__)"],
    ["beautifulsoup4", "import bs4; print(bs4.__version__)"],
    ["networkx", "import networkx; print(networkx.__version__)"],
    ["jsonschema", "import jsonschema; print(jsonschema.__version__)"],
    ["idna", "import idna; print(idna.__version__)"],
    ["certifi", "import certifi; print(certifi.__version__)"],
    ["pydantic", "import pydantic; print(pydantic.__version__)"],
  ]) {
    it(pkg, { skip: SKIP, timeout: 120_000 }, async () => {
      const s = await openSession();
      await s.installPythonPackages(pkg);
      const r = await s.run(`python3 -c "${importCheck}"`);
      assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
      assert.ok(r.stdout.trim().length > 0, "should print version");
    });
  }

  // ── C extension with pure-Python fallback ────────────────────

  it("pyyaml", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openSession();
    await s.installPythonPackages("pyyaml");
    const r = await s.run('python3 -c "import yaml; print(yaml.__version__)"');
    assert.equal(r.exitCode, 0, `import failed: ${r.stderr}`);
  });

  // ── C extension packages (need MAIN_MODULE=1 for dlopen) ─────
  // Install succeeds but import fails because .so side modules need
  // CPython symbols exported via MAIN_MODULE=1.

  for (const [pkg, importMod] of [
    ["regex", "regex"],
    ["numpy", "numpy"],
    ["pandas", "pandas"],
    ["scipy", "scipy"],
  ]) {
    it(`${pkg} — install succeeds, import needs MAIN_MODULE=1`, { skip: SKIP, timeout: 180_000 }, async () => {
      const s = await openSession();
      const result = await s.installPythonPackages(pkg);
      assert.ok(result.installed.length > 0, "install should succeed");
      const r = await s.run(`python3 -c "import ${importMod}"`);
      // Currently fails: dlopen can't resolve CPython symbols
      assert.notEqual(r.exitCode, 0, "expected import failure with MAIN_MODULE=2");
    });
  }
});
