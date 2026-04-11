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
    "build-component",
    "clippy-component",
    "test-e2e-component",
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

describe("README documents all three build paths", () => {
  it("mentions standalone, pyodide, and component", () => {
    const readme = readFileSync(resolve(REPO, "README.md"), "utf-8");
    assert.ok(readme.includes("Standalone"), "README should mention Standalone");
    assert.ok(readme.includes("Pyodide"), "README should mention Pyodide");
    assert.ok(
      readme.includes("Component Model") || readme.includes("wasm32-wasip2"),
      "README should mention the Component Model / WASI P2 transport",
    );
  });
});

describe("Component Model transport plumbing", () => {
  it("rust-toolchain.toml lists wasm32-wasip2", () => {
    const content = readFileSync(resolve(REPO, "rust-toolchain.toml"), "utf-8");
    assert.ok(
      content.includes("wasm32-wasip2"),
      "rust-toolchain.toml should list wasm32-wasip2",
    );
  });

  it("workspace Cargo.toml lists the wasmsh-component member", () => {
    const content = readFileSync(resolve(REPO, "Cargo.toml"), "utf-8");
    assert.ok(
      content.includes("crates/wasmsh-component"),
      "workspace Cargo.toml should include crates/wasmsh-component",
    );
  });

  it("component crate has a wit/ world file", () => {
    assert.ok(
      existsSync(resolve(REPO, "crates/wasmsh-component/wit/world.wit")),
      "wasmsh-component should ship a wit/world.wit",
    );
  });

  it("CLAUDE.md documents the new component target", () => {
    const content = readFileSync(resolve(REPO, "CLAUDE.md"), "utf-8");
    assert.ok(
      content.includes("wasm32-wasip2") || content.includes("wasmsh-component"),
      "CLAUDE.md should mention the WASI P2 component target",
    );
  });

  it("docs/reference/protocol.md mentions the third transport", () => {
    const content = readFileSync(
      resolve(REPO, "docs/reference/protocol.md"),
      "utf-8",
    );
    assert.ok(
      content.includes("Component Model") || content.includes("wasm32-wasip2"),
      "docs/reference/protocol.md should describe the Component Model transport",
    );
  });

  it("protocol docs describe component transport as Pyodide-parity JSON, not a bespoke session API", () => {
    const content = readFileSync(
      resolve(REPO, "docs/reference/protocol.md"),
      "utf-8",
    );
    assert.ok(
      content.includes("Pyodide") && content.includes("JSON"),
      "protocol docs should describe the component transport as a Pyodide-parity JSON surface",
    );
    assert.ok(
      !content.includes("resource session"),
      "protocol docs must not describe the component transport as a session resource",
    );
    assert.ok(
      !content.includes("run-once"),
      "protocol docs must not mention a run-once helper for the component transport",
    );
    assert.ok(
      !content.includes("wasmsh:component/sandbox"),
      "protocol docs must not describe the old sandbox WIT package as the transport contract",
    );
  });

  it("ADR-0030 describes the thin runtime handle contract", () => {
    const content = readFileSync(
      resolve(REPO, "docs/adr/adr-0030-wasmcloud-component-transport.md"),
      "utf-8",
    );
    assert.ok(
      content.includes("handle-json"),
      "ADR-0030 should document the handle-json component surface",
    );
    assert.ok(
      content.includes("probe-version"),
      "ADR-0030 should document the shared probe surface",
    );
    assert.ok(
      !content.includes("resource session"),
      "ADR-0030 must not keep the old session-resource contract",
    );
    assert.ok(
      !content.includes("run-once"),
      "ADR-0030 must not keep the old run-once helper",
    );
  });
});
