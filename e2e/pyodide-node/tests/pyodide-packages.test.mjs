/**
 * Smoke tests for popular Pyodide packages installed via micropip.
 *
 * These use the real micropip (pre-installed via loadPyodide) to install
 * packages from the Pyodide CDN or PyPI into the sandbox.  Both pure-Python
 * packages and packages that ship a compiled `.so` side module are expected
 * to import cleanly.
 *
 * If a compiled-package import here regresses with
 *   `bad export type for 'PyExc_…': undefined`
 * the custom Pyodide wasm has lost its MAIN_MODULE=1 dynamic-linking
 * support — see `tools/pyodide/build-custom.sh` and the emscripten.py
 * patch there.
 *
 * The final `numpy + pandas + scipy` compute test runs a real calculation
 * across all three libraries.  It is the regression-catcher for subtler
 * failures where top-level `import` succeeds but inner functions fail to
 * resolve a symbol at first use.
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

// Shared assertion helper: every `session.run()` result includes both
// stdout and stderr plus the exit code in the failure message, so a
// regression produces a useful diagnostic instead of `"import failed: "`.
function assertRunOk(r, label) {
  assert.equal(
    r.exitCode,
    0,
    `${label} failed (exit=${r.exitCode})\n` +
      `stdout: ${r.stdout || "<empty>"}\n` +
      `stderr: ${r.stderr || "<empty>"}`,
  );
}

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
      assertRunOk(r, `${pkg} import`);
      assert.ok(r.stdout.trim().length > 0, "should print version");
    });
  }

  // ── C extension with pure-Python fallback ────────────────────
  //
  // pyyaml ships both a C extension (`_yaml`) and a pure-Python
  // fallback.  A regression in compiled-side-module loading can
  // silently fall back to the slow pure-Python loader, which passes
  // `import yaml` but defeats the whole point of this test suite —
  // so we assert the C loader was actually picked up.

  it("pyyaml (C extension loader)", { skip: SKIP, timeout: 120_000 }, async () => {
    const s = await openNetworkSession();
    await s.installPythonPackages("pyyaml");
    const r = await s.run(
      `python3 -c "import yaml, _yaml; print('yaml=', yaml.__version__); print('loader=', _yaml.__name__)"`,
    );
    assertRunOk(r, "pyyaml import");
    assert.ok(
      r.stdout.includes("loader= _yaml"),
      `expected _yaml C extension to be importable; got stdout: ${r.stdout}`,
    );
  });

  // ── Compiled C extension packages (load .so side modules) ────
  // These ship .cpython-*-pyodide_*-wasm32.so binaries that resolve
  // CPython / libc / libstdc++ symbols at runtime against the main
  // module. They only work because the wasm is built with MAIN_MODULE=1.
  //
  // The sentinel value printed by each test is chosen to be impossible
  // to match by accident inside a Python traceback or warning message,
  // so a regression that somehow produces the substring "ok" can't
  // silently pass.

  const SIDE_MODULE_SENTINEL = "WASMSH_SIDE_MODULE_OK_8f2c1e4d";

  for (const [pkg, importMod] of [
    ["jsonschema", "jsonschema"],
    ["pydantic", "pydantic"],
    ["regex", "regex"],
    ["numpy", "numpy"],
    ["pandas", "pandas"],
    ["scipy", "scipy"],
  ]) {
    it(`${pkg} — install + import compiled side module`, {
      skip: SKIP,
      timeout: 180_000,
    }, async () => {
      const s = await openNetworkSession();
      const result = await s.installPythonPackages(pkg);
      assert.ok(
        result.installed.length > 0,
        `install should succeed (got ${JSON.stringify(result)})`,
      );
      const r = await s.run(
        `python3 -c "import ${importMod}; print('${SIDE_MODULE_SENTINEL}')"`,
      );
      assertRunOk(r, `${pkg} import`);
      // Exact last-line match rather than substring — avoids matching
      // the sentinel inside a traceback line or deprecation warning.
      const lastLine = r.stdout.trim().split("\n").pop();
      assert.equal(
        lastLine,
        SIDE_MODULE_SENTINEL,
        `expected final stdout line to be the sentinel; got: ${r.stdout}`,
      );
    });
  }

  // ── Real computation across numpy + pandas + scipy ───────────
  //
  // The per-package "import and print sentinel" tests above catch the
  // canonical "top-level import fails to resolve a symbol" regression
  // (this was the v0.5.7 breakage).  But compiled side modules can fail
  // in subtler ways — e.g. import succeeds because it lazily maps
  // symbols, and the first real function call blows up with a missing
  // `PyExc_*` or libstdc++ typeinfo.  Running a non-trivial computation
  // across all three libraries exercises the call graph that a pure
  // import does not reach.

  it(
    "numpy + pandas + scipy compute a real result",
    { skip: SKIP, timeout: 240_000 },
    async () => {
      const s = await openNetworkSession();
      await s.installPythonPackages(["numpy", "pandas", "scipy"]);
      const r = await s.run(
        `python3 -c "
import numpy as np
import pandas as pd
from scipy import stats

a = np.arange(10, dtype=float)
b = a * 2.0 + 1.0
df = pd.DataFrame({'a': a, 'b': b})

sum_a = int(df['a'].sum())
mean_b = float(df['b'].mean())
corr, _ = stats.pearsonr(df['a'], df['b'])

print(f'sum_a={sum_a} mean_b={mean_b:.2f} corr={corr:.6f}')
"`,
      );
      assertRunOk(r, "numpy+pandas+scipy compute");
      // The expected output is fully deterministic.  Substring match
      // only on the computation result line (deprecation warnings
      // from scipy can still appear in earlier stdout lines).
      assert.ok(
        r.stdout.includes("sum_a=45 mean_b=10.00 corr=1.000000"),
        `compute result mismatch; got: ${r.stdout}`,
      );
    },
  );
});
