/**
 * Packaging smoke tests for the wasmsh-pyodide-runtime Python package.
 *
 * These verify the package layout, metadata, and asset parity with the
 * npm package without requiring a Pyodide build.
 */
import { describe, it } from "node:test";
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PY_PKG_DIR = resolve(
  __dirname,
  "../../packages/python/wasmsh-pyodide-runtime",
);
const NPM_PKG_DIR = resolve(__dirname, "../../packages/npm/wasmsh-pyodide");

describe("python package: pyproject.toml", () => {
  const toml = readFileSync(resolve(PY_PKG_DIR, "pyproject.toml"), "utf-8");

  it("has correct package name", () => {
    assert.ok(toml.includes('name = "wasmsh-pyodide-runtime"'));
  });

  it("has version", () => {
    assert.ok(toml.includes("version ="));
  });

  it("requires Python >= 3.11", () => {
    assert.ok(toml.includes('requires-python = ">=3.11"'));
  });

  it("uses setuptools build backend", () => {
    assert.ok(toml.includes("setuptools"));
  });

  it("includes assets in package data", () => {
    assert.ok(toml.includes('"assets/*"'));
  });
});

describe("python package: module structure", () => {
  const modDir = resolve(PY_PKG_DIR, "wasmsh_pyodide_runtime");

  it("__init__.py exists", () => {
    assert.ok(existsSync(resolve(modDir, "__init__.py")));
  });

  it("locator.py exists", () => {
    assert.ok(existsSync(resolve(modDir, "locator.py")));
  });

  it("__init__.py exports get_dist_dir", () => {
    const content = readFileSync(resolve(modDir, "__init__.py"), "utf-8");
    assert.ok(content.includes("get_dist_dir"));
  });

  it("__init__.py exports get_asset_path", () => {
    const content = readFileSync(resolve(modDir, "__init__.py"), "utf-8");
    assert.ok(content.includes("get_asset_path"));
  });

  it("__init__.py exports get_node_host_script", () => {
    const content = readFileSync(resolve(modDir, "__init__.py"), "utf-8");
    assert.ok(content.includes("get_node_host_script"));
  });
});

describe("python package: locator.py functions", () => {
  const locator = readFileSync(
    resolve(PY_PKG_DIR, "wasmsh_pyodide_runtime/locator.py"),
    "utf-8",
  );

  it("defines get_dist_dir", () => {
    assert.ok(locator.includes("def get_dist_dir"));
  });

  it("defines get_asset_path", () => {
    assert.ok(locator.includes("def get_asset_path"));
  });

  it("defines get_node_host_script", () => {
    assert.ok(locator.includes("def get_node_host_script"));
  });

  it("get_node_host_script returns path inside assets", () => {
    assert.ok(
      locator.includes("node-host.mjs"),
      "get_node_host_script should reference node-host.mjs",
    );
  });
});

describe("python package: README exists", () => {
  it("has README.md", () => {
    assert.ok(existsSync(resolve(PY_PKG_DIR, "README.md")));
  });
});

describe("packaging script", () => {
  const script = resolve(
    __dirname,
    "../../tools/pyodide/package-runtime-assets.mjs",
  );

  it("exists", () => {
    assert.ok(existsSync(script));
  });

  it("copies to npm assets directory", () => {
    const content = readFileSync(script, "utf-8");
    assert.ok(content.includes("wasmsh-pyodide"));
    assert.ok(content.includes("assets"));
  });

  it("copies node-host.mjs to python assets", () => {
    const content = readFileSync(script, "utf-8");
    assert.ok(
      content.includes("node-host.mjs"),
      "should copy node-host.mjs into Python assets",
    );
  });

  it("copies lib/ to python assets", () => {
    const content = readFileSync(script, "utf-8");
    assert.ok(
      content.includes('"lib"'),
      "should copy lib/ into Python assets",
    );
  });
});
