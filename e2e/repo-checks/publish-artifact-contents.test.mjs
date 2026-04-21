/**
 * Built-artifact contents guards.
 *
 * The `python-runtime-package.test.mjs` parity tests verify that the
 * source trees of the npm and Python packages agree.  They do NOT
 * verify that everything in the source tree actually ends up in the
 * published wheel or tarball — which is exactly how the 0.5.10-0.6.2
 * line of wheels shipped without `assets/lib/baseline/boot-plan.mjs`
 * (setuptools' fnmatch-based `package-data` globs are not recursive,
 * so a non-matching subdirectory silently disappears at build time).
 *
 * These tests close that gap: build the two published artifacts in a
 * temporary directory and assert that every file the runtime actually
 * imports at runtime is present.  A future regression in
 * `pyproject.toml` (wheel) or `package.json#files` (tarball) fails CI
 * instead of shipping.
 *
 * Both tests skip cleanly when the packaging tool isn't on PATH so
 * the suite still runs on contributor machines that have Node but not
 * `python -m build` / pnpm configured.
 */
import { describe, it, before } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../..");
const PY_PKG_DIR = resolve(REPO_ROOT, "packages/python/wasmsh-pyodide-runtime");
const NPM_PKG_DIR = resolve(REPO_ROOT, "packages/npm/wasmsh-pyodide");

// Source of truth: every file path below must exist in the published
// artifact of each ecosystem.  The npm package keeps the runtime
// glue scripts at the top-level `lib/` and the binary assets under
// `assets/`; the Python package nests everything inside
// `wasmsh_pyodide_runtime/assets/`, so the same logical file shows
// up at two different paths.  Track both.
//
// When a new runtime module or binary asset is added, add it here.
// (This is also how we caught that assets/lib/baseline/*.mjs was
// missing from every wheel since 0.5.10.)
const REQUIRED_ASSET_PATHS = [
  // logical path → { npm: "x", python: "y" }
  { logical: "lib/allowlist.mjs",         npm: "lib/allowlist.mjs",         python: "assets/lib/allowlist.mjs" },
  { logical: "lib/fetch-helper.mjs",      npm: "lib/fetch-helper.mjs",      python: "assets/lib/fetch-helper.mjs" },
  { logical: "lib/node-module.mjs",       npm: "lib/node-module.mjs",       python: "assets/lib/node-module.mjs" },
  { logical: "lib/protocol.mjs",          npm: "lib/protocol.mjs",          python: "assets/lib/protocol.mjs" },
  { logical: "lib/runtime-bridge.mjs",    npm: "lib/runtime-bridge.mjs",    python: "assets/lib/runtime-bridge.mjs" },
  { logical: "lib/baseline/boot-plan.mjs", npm: "lib/baseline/boot-plan.mjs", python: "assets/lib/baseline/boot-plan.mjs" },
  { logical: "pyodide-lock.json",         npm: "assets/pyodide-lock.json",  python: "assets/pyodide-lock.json" },
  { logical: "pyodide.asm.wasm",          npm: "assets/pyodide.asm.wasm",   python: "assets/pyodide.asm.wasm" },
];

function which(cmd) {
  const probe = spawnSync("sh", ["-c", `command -v ${cmd}`], { encoding: "utf-8" });
  return probe.status === 0 && probe.stdout.trim().length > 0;
}

describe("built python wheel", () => {
  const pythonAvailable = which("python3") && which("uv");
  const sourceAssetsPresent = existsSync(
    resolve(PY_PKG_DIR, "wasmsh_pyodide_runtime/assets/pyodide.asm.wasm"),
  );

  let wheelRoot;

  before(() => {
    if (!pythonAvailable) return;
    if (!sourceAssetsPresent) return;
    wheelRoot = mkdtempSync(join(tmpdir(), "wasmsh-wheel-check-"));
    // `uv build --wheel` is fast and doesn't need a Python venv; stdout
    // is noisy so funnel it into the temp dir.
    execFileSync("uv", ["build", "--wheel", "--out-dir", wheelRoot], {
      cwd: PY_PKG_DIR,
      stdio: ["ignore", "pipe", "pipe"],
    });
  });

  for (const entry of REQUIRED_ASSET_PATHS) {
    it(`wheel includes ${entry.python}`, (t) => {
      if (!pythonAvailable) {
        t.skip("python3 + uv are required to build the wheel");
        return;
      }
      if (!sourceAssetsPresent) {
        t.skip(
          "built Pyodide assets not staged in the python package (run `just build-pyodide && just package-pyodide-runtime`)",
        );
        return;
      }
      assert.ok(wheelRoot, "wheel root should be populated by before()");
      const [wheelName] = readdirSync(wheelRoot).filter((name) => name.endsWith(".whl"));
      assert.ok(wheelName, `no .whl produced in ${wheelRoot}`);

      // A wheel is a zip — `python3 -m zipfile -l foo.whl` lists the
      // entries portably without needing the system unzip.
      const wheelPath = resolve(wheelRoot, wheelName);
      const listing = execFileSync("python3", ["-m", "zipfile", "-l", wheelPath], {
        encoding: "utf-8",
      });
      const expectedEntry = `wasmsh_pyodide_runtime/${entry.python}`;
      assert.ok(
        listing.includes(expectedEntry),
        [
          `wheel is missing ${expectedEntry}`,
          "This usually means `[tool.setuptools.package-data]` in",
          "packages/python/wasmsh-pyodide-runtime/pyproject.toml does not",
          "match the actual asset tree — globs there are non-recursive.",
          "",
          "Wheel contents:",
          listing,
        ].join("\n"),
      );
    });
  }

  // eslint-disable-next-line no-unused-vars
  const _cleanup = () => {
    if (wheelRoot) {
      rmSync(wheelRoot, { recursive: true, force: true });
    }
  };
});

describe("built npm tarball", () => {
  const pnpmAvailable = which("pnpm");
  const sourceAssetsPresent = existsSync(resolve(NPM_PKG_DIR, "assets/pyodide.asm.wasm"));

  let tarballFiles;

  before(() => {
    if (!pnpmAvailable) return;
    if (!sourceAssetsPresent) return;
    // `pnpm pack --pack-destination` emits a tarball; we use `--json`
    // on `npm pack --dry-run` to get the file list without actually
    // writing a tarball or mutating state.
    const out = execFileSync("npm", ["pack", "--dry-run", "--json"], {
      cwd: NPM_PKG_DIR,
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    const parsed = JSON.parse(out);
    tarballFiles = parsed[0].files.map((entry) => entry.path);
  });

  for (const entry of REQUIRED_ASSET_PATHS) {
    it(`tarball includes ${entry.npm}`, (t) => {
      if (!pnpmAvailable) {
        t.skip("pnpm + npm are required to verify the tarball layout");
        return;
      }
      if (!sourceAssetsPresent) {
        t.skip(
          "built Pyodide assets not staged in the npm package (run `just build-pyodide && just package-pyodide-runtime`)",
        );
        return;
      }
      assert.ok(tarballFiles, "tarball file listing should be populated");
      assert.ok(
        tarballFiles.includes(entry.npm),
        [
          `npm tarball is missing ${entry.npm}`,
          "Check the `files` glob in packages/npm/wasmsh-pyodide/package.json.",
          "",
          "Tarball contents:",
          tarballFiles.join("\n"),
        ].join("\n"),
      );
    });
  }
});
