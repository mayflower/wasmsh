/**
 * Repo-level checks: verifies that all documented build/test just targets
 * exist and that version pins are consistent across the dual-build system.
 */
import { describe, it } from "node:test";
import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(__dirname, "../..");

function justTargetExists(target) {
  const justfile = readFileSync(resolve(REPO, "justfile"), "utf-8");
  return justfile.includes(target + ":");
}

describe("just targets exist", () => {
  for (const target of [
    "build-standalone",
    "test-e2e-standalone",
    "build-pyodide",
    "test-e2e-pyodide-node",
    "test-e2e-pyodide-browser",
    "test-e2e-kind",
    "test-e2e-dispatcher-compose",
  ]) {
    it("just " + target, () => {
      assert.ok(justTargetExists(target), "missing just target: " + target);
    });
  }
});

describe("version pin consistency", () => {
  it("tools/pyodide/versions.env exists and has PYODIDE_VERSION", () => {
    const f = resolve(REPO, "tools/pyodide/versions.env");
    assert.ok(existsSync(f), "tools/pyodide/versions.env not found");
    const content = readFileSync(f, "utf-8");
    assert.ok(content.includes("PYODIDE_VERSION="), "missing PYODIDE_VERSION");
    assert.ok(content.includes("EMSCRIPTEN_VERSION="), "missing EMSCRIPTEN_VERSION");
  });

  it("build-custom.sh reads from versions.env", () => {
    const f = resolve(REPO, "tools/pyodide/build-custom.sh");
    const content = readFileSync(f, "utf-8");
    assert.ok(content.includes("versions.env"), "build-custom.sh should source versions.env");
  });
});

describe("ADRs exist", () => {
  for (const adr of [
    "ADR-0017-shared-runtime-extraction.md",
    "ADR-0018-pyodide-same-module.md",
    "ADR-0019-dual-target-packaging.md",
    "ADR-0020-e2e-first-testing.md",
    "adr-0030-wasmcloud-component-transport.md",
  ]) {
    it(adr, () => {
      assert.ok(
        existsSync(resolve(REPO, "docs/adr", adr)),
        "missing ADR: " + adr,
      );
    });
  }
});

describe("README documents all build paths", () => {
  it("mentions standalone, pyodide, and scalable", () => {
    const readme = readFileSync(resolve(REPO, "README.md"), "utf-8");
    assert.ok(readme.includes("Standalone"), "README should mention Standalone");
    assert.ok(readme.includes("Pyodide"), "README should mention Pyodide");
    assert.ok(
      readme.includes("Scalable") || readme.includes("wasmsh-dispatcher"),
      "README should mention the scalable dispatcher/runner path",
    );
  });
});

describe("scalable deployment plumbing", () => {
  it("workspace Cargo.toml lists wasmsh-dispatcher and wasmsh-json-bridge", () => {
    const content = readFileSync(resolve(REPO, "Cargo.toml"), "utf-8");
    assert.ok(
      content.includes("crates/wasmsh-dispatcher"),
      "workspace Cargo.toml should include crates/wasmsh-dispatcher",
    );
    assert.ok(
      content.includes("crates/wasmsh-json-bridge"),
      "workspace Cargo.toml should include crates/wasmsh-json-bridge",
    );
  });

  it("helm chart exists", () => {
    assert.ok(
      existsSync(resolve(REPO, "deploy/helm/wasmsh/Chart.yaml")),
      "deploy/helm/wasmsh should ship a Chart.yaml",
    );
  });

  it("dispatcher HTTP contract is documented", () => {
    assert.ok(
      existsSync(resolve(REPO, "docs/reference/dispatcher-api.md")),
      "docs/reference/dispatcher-api.md should document the HTTP surface",
    );
  });

  it("both e2e orchestrators exist", () => {
    assert.ok(
      existsSync(resolve(REPO, "e2e/kind/scripts/run.mjs")),
      "e2e/kind/scripts/run.mjs should exist",
    );
    assert.ok(
      existsSync(resolve(REPO, "e2e/dispatcher-compose/scripts/run.mjs")),
      "e2e/dispatcher-compose/scripts/run.mjs should exist",
    );
  });

  it("LangChain adapters export WasmshRemoteSandbox", () => {
    const tsIndex = readFileSync(
      resolve(REPO, "packages/npm/langchain-wasmsh/src/index.ts"),
      "utf-8",
    );
    assert.ok(
      tsIndex.includes("WasmshRemoteSandbox"),
      "TS adapter should export WasmshRemoteSandbox",
    );
    const pyInit = readFileSync(
      resolve(REPO, "packages/python/langchain-wasmsh/langchain_wasmsh/__init__.py"),
      "utf-8",
    );
    assert.ok(
      pyInit.includes("WasmshRemoteSandbox"),
      "Python adapter should export WasmshRemoteSandbox",
    );
  });
});
