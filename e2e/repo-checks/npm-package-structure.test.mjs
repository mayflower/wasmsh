/**
 * Packaging smoke tests for the wasmsh-pyodide npm package.
 *
 * These verify the package layout, exports, and TypeScript declarations
 * without requiring a Pyodide build or any runtime execution.
 */
import { describe, it } from "node:test";
import { readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../packages/npm/wasmsh-pyodide");

describe("npm package: package.json", () => {
  const pkg = JSON.parse(readFileSync(resolve(PKG_DIR, "package.json"), "utf-8"));

  it("has correct package name", () => {
    assert.equal(pkg.name, "@mayflowergmbh/wasmsh-pyodide");
  });

  it("exports root entry", () => {
    assert.deepEqual(pkg.exports["."], {
      types: "./index.d.ts",
      browser: "./browser.js",
      import: "./index.js",
      default: "./index.js",
    });
  });

  it("exports browser subpath", () => {
    assert.deepEqual(pkg.exports["./browser"], {
      types: "./index.d.ts",
      default: "./browser.js",
    });
  });

  it("exports node subpath", () => {
    assert.deepEqual(pkg.exports["./node"], {
      types: "./index.d.ts",
      default: "./index.js",
    });
  });

  it("exports node-host subpath", () => {
    assert.equal(pkg.exports["./node-host"], "./node-host.mjs");
  });

  it("exports browser-worker subpath", () => {
    assert.equal(pkg.exports["./browser-worker"], "./browser-worker.js");
  });

  it("requires Node >= 20", () => {
    assert.equal(pkg.engines.node, ">=20");
  });

  it("files array includes assets", () => {
    assert.ok(pkg.files.includes("assets/**/*"), "should include assets/**/*");
  });
});

describe("npm package: index.js exports", () => {
  it("exports createNodeSession", async () => {
    const mod = await import(resolve(PKG_DIR, "index.js"));
    assert.equal(typeof mod.createNodeSession, "function");
  });

  it("exports createBrowserWorkerSession", async () => {
    const mod = await import(resolve(PKG_DIR, "index.js"));
    assert.equal(typeof mod.createBrowserWorkerSession, "function");
  });

  it("exports resolveAssetPath", async () => {
    const mod = await import(resolve(PKG_DIR, "index.js"));
    assert.equal(typeof mod.resolveAssetPath, "function");
  });

  it("exports resolveNodeHostPath", async () => {
    const mod = await import(resolve(PKG_DIR, "index.js"));
    assert.equal(typeof mod.resolveNodeHostPath, "function");
  });

  it("exports DEFAULT_WORKSPACE_DIR as /workspace", async () => {
    const mod = await import(resolve(PKG_DIR, "index.js"));
    assert.equal(mod.DEFAULT_WORKSPACE_DIR, "/workspace");
  });
});

describe("npm package: browser.js exports", () => {
  it("file exists", () => {
    assert.ok(existsSync(resolve(PKG_DIR, "browser.js")));
  });

  it("does not import node builtins", () => {
    const content = readFileSync(resolve(PKG_DIR, "browser.js"), "utf-8");
    assert.ok(!content.includes("node:child_process"));
    assert.ok(!content.includes("node:path"));
    assert.ok(!content.includes("node:readline"));
    assert.ok(!content.includes("node:url"));
  });

  it("exports createBrowserWorkerSession", async () => {
    const mod = await import(resolve(PKG_DIR, "browser.js"));
    assert.equal(typeof mod.createBrowserWorkerSession, "function");
  });

  it("exports createNodeSession stub", async () => {
    const mod = await import(resolve(PKG_DIR, "browser.js"));
    assert.equal(typeof mod.createNodeSession, "function");
  });

  it("exports resolveBrowserWorkerPath", async () => {
    const mod = await import(resolve(PKG_DIR, "browser.js"));
    assert.equal(typeof mod.resolveBrowserWorkerPath, "function");
  });
});

describe("npm package: index.d.ts declarations", () => {
  const dts = readFileSync(resolve(PKG_DIR, "index.d.ts"), "utf-8");

  it("declares WasmshSession interface", () => {
    assert.ok(dts.includes("WasmshSession"), "should declare WasmshSession");
  });

  it("declares NodeSessionOptions", () => {
    assert.ok(dts.includes("NodeSessionOptions"), "should declare NodeSessionOptions");
  });

  it("declares BrowserSessionOptions", () => {
    assert.ok(dts.includes("BrowserSessionOptions"), "should declare BrowserSessionOptions");
  });

  it("declares RunResult interface", () => {
    assert.ok(dts.includes("RunResult"), "should declare RunResult");
  });

  it("declares createNodeSession function", () => {
    assert.ok(dts.includes("createNodeSession"), "should declare createNodeSession");
  });

  it("declares createBrowserWorkerSession function", () => {
    assert.ok(dts.includes("createBrowserWorkerSession"), "should declare createBrowserWorkerSession");
  });
});

describe("npm package: node-host.mjs", () => {
  it("file exists", () => {
    assert.ok(existsSync(resolve(PKG_DIR, "node-host.mjs")));
  });

  it("does not import from ./index.js", () => {
    const content = readFileSync(resolve(PKG_DIR, "node-host.mjs"), "utf-8");
    assert.ok(
      !content.includes('from "./index.js"'),
      "node-host.mjs must not import from ./index.js (breaks Python package path)",
    );
  });

  it("imports from ./lib/node-module.mjs", () => {
    const content = readFileSync(resolve(PKG_DIR, "node-host.mjs"), "utf-8");
    assert.ok(content.includes('./lib/node-module.mjs'));
  });
});

describe("npm package: browser-worker.js", () => {
  it("file exists", () => {
    assert.ok(existsSync(resolve(PKG_DIR, "browser-worker.js")));
  });

  it("handles onmessage events", () => {
    const content = readFileSync(resolve(PKG_DIR, "browser-worker.js"), "utf-8");
    assert.ok(content.includes("self.onmessage"), "should set self.onmessage");
  });
});

describe("npm package: lib/node-module.mjs", () => {
  it("file exists", () => {
    assert.ok(existsSync(resolve(PKG_DIR, "lib/node-module.mjs")));
  });

  it("exports createFullModule", async () => {
    const mod = await import(resolve(PKG_DIR, "lib/node-module.mjs"));
    assert.equal(typeof mod.createFullModule, "function");
  });
});
