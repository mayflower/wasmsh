import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");
const HAS_ASSETS = existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

const {
  buildBaselineBootPlan,
} = await import(resolve(PKG_DIR, "lib/baseline/boot-plan.mjs"));
const {
  assertOfflineBaselineBootPlan,
} = await import(resolve(PKG_DIR, "lib/baseline/offline-guard.mjs"));
const {
  createSandboxGlobalSurface,
} = await import(resolve(PKG_DIR, "lib/baseline/sandbox-globals.mjs"));
const {
  composeWasmImports,
} = await import(resolve(PKG_DIR, "lib/baseline/import-composer.mjs"));

describe("baseline boot contract", () => {
  it("baseline boot plan excludes optional packages and dynamic installs", () => {
    const plan = buildBaselineBootPlan({ assetDir: ASSETS_DIR });

    assert.equal(plan.network_required, false);
    assert.deepEqual(plan.optional_python_packages, []);
    assert.deepEqual(plan.dynamic_install_steps, []);
    assert.equal(plan.restore_strategy, "baseline-boot");
    assert.doesNotThrow(() => assertOfflineBaselineBootPlan(plan));
  });

  it("sandbox global surface hides host capability globals", () => {
    const surface = createSandboxGlobalSurface({
      console,
      require() {},
      process: {},
      Deno: {},
      fetch() {},
      WebSocket: class {},
      TextEncoder,
    });

    for (const forbidden of ["require", "process", "Deno", "fetch", "WebSocket"]) {
      assert.equal(forbidden in surface, false, `${forbidden} must not be exposed`);
    }
    assert.equal("console" in surface, true);
    assert.equal("TextEncoder" in surface, true);
  });

  it("composeWasmImports is local and does not patch global instantiate", () => {
    const before = WebAssembly.instantiate;
    const imports = composeWasmImports({
      env: { sample_env_import: 1 },
      sentinel: { create_sentinel: () => Symbol("sentinel") },
    });

    assert.equal(WebAssembly.instantiate, before);
    assert.equal(imports.env.sample_env_import, 1);
    assert.equal(typeof imports.sentinel.create_sentinel, "function");
  });

  it("node boot leaves global WebAssembly.instantiate unchanged", {
    skip: !HAS_ASSETS,
    timeout: 120_000,
  }, async () => {
    const { createNodeSession } = await import(resolve(PKG_DIR, "index.js"));
    const before = WebAssembly.instantiate;
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    try {
      assert.equal(WebAssembly.instantiate, before);
    } finally {
      await session.close();
    }
  });
});
