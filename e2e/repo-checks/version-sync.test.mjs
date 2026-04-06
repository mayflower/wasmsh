/**
 * Verify that all package versions in the monorepo are in sync.
 *
 * Catches drift between Cargo.toml, package.json, and pyproject.toml
 * before it reaches a release.
 */
import { describe, it } from "node:test";
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "../..");

function readToml(path) {
  return readFileSync(resolve(ROOT, path), "utf-8");
}

function extractTomlVersion(content) {
  const m = content.match(/^version\s*=\s*"([^"]+)"/m);
  return m ? m[1] : null;
}

function extractJsonVersion(path) {
  const pkg = JSON.parse(readFileSync(resolve(ROOT, path), "utf-8"));
  return pkg.version;
}

describe("version sync across all packages", () => {
  const cargoWorkspace = extractTomlVersion(readToml("Cargo.toml"));

  it("workspace Cargo.toml has a version", () => {
    assert.ok(cargoWorkspace, "Cargo.toml should have a version field");
  });

  it("wasmsh-pyodide-probe matches workspace", () => {
    const v = extractTomlVersion(
      readToml("crates/wasmsh-pyodide-probe/Cargo.toml"),
    );
    assert.equal(v, cargoWorkspace, "wasmsh-pyodide-probe version mismatch");
  });

  it("wasmsh-pyodide matches workspace", () => {
    const v = extractTomlVersion(
      readToml("crates/wasmsh-pyodide/Cargo.toml"),
    );
    assert.equal(v, cargoWorkspace, "wasmsh-pyodide version mismatch");
  });

  it("npm package matches workspace", () => {
    const v = extractJsonVersion(
      "packages/npm/wasmsh-pyodide/package.json",
    );
    assert.equal(v, cargoWorkspace, "npm package version mismatch");
  });

  it("python package matches workspace", () => {
    const v = extractTomlVersion(
      readToml("packages/python/wasmsh-pyodide-runtime/pyproject.toml"),
    );
    assert.equal(v, cargoWorkspace, "python package version mismatch");
  });
});
