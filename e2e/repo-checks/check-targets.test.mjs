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
  ]) {
    it(adr, () => {
      assert.ok(
        existsSync(resolve(REPO, "docs/adr", adr)),
        "missing ADR: " + adr,
      );
    });
  }
});

describe("README documents both build paths", () => {
  it("mentions standalone and pyodide", () => {
    const readme = readFileSync(resolve(REPO, "README.md"), "utf-8");
    assert.ok(readme.includes("Standalone"), "README should mention Standalone");
    assert.ok(readme.includes("Pyodide"), "README should mention Pyodide");
  });
});
