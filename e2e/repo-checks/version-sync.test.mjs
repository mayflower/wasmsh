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

  it("helm chart appVersion matches workspace", () => {
    // Chart.yaml has two version fields with intentionally different
    // semantics: `version:` is the chart API version (bumped when the
    // values schema or template surface changes), `appVersion:` tracks
    // the app the chart deploys.  Only the latter needs to stay in
    // lockstep with the workspace — `tools/bump-version.sh` handles it.
    const chart = readToml("deploy/helm/wasmsh/Chart.yaml");
    const m = chart.match(/^appVersion:\s*"([^"]+)"/m);
    assert.ok(m, "Chart.yaml should have an appVersion field");
    assert.equal(
      m[1],
      cargoWorkspace,
      "Chart.yaml appVersion drifted from workspace (run tools/bump-version.sh)",
    );
  });

  it("workspace internal dep pins match workspace version", () => {
    // `[workspace.dependencies]` has one entry per internal crate in
    // the form `wasmsh-foo = { version = "X.Y.Z", path = "..." }`.
    // `cargo publish` consumes the `version` field (not `path`), so any
    // drift between these pins and the workspace package version means
    // published crates pin each other at a stale version number.
    // `tools/bump-version.sh` bumps both in lockstep — this test catches
    // a future change that forgets to keep them aligned.
    const cargo = readToml("Cargo.toml");
    const re = /^(wasmsh-[a-z_-]+)\s*=\s*\{\s*version\s*=\s*"([^"]+)"/gm;
    const mismatches = [];
    for (const [, name, version] of cargo.matchAll(re)) {
      if (version !== cargoWorkspace) {
        mismatches.push(`${name} pinned at ${version} (expected ${cargoWorkspace})`);
      }
    }
    assert.equal(
      mismatches.length,
      0,
      `workspace internal dep pins out of sync with workspace version ${cargoWorkspace}:\n  ` +
        mismatches.join("\n  "),
    );
  });
});
