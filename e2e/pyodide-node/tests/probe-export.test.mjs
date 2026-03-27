/**
 * Verifies that the custom Pyodide build contains the Rust probe function
 * inside the main Emscripten module (same-module integration, not side-loading).
 *
 * Skip when the custom build is unavailable:
 *   SKIP_PYODIDE=1 node --test e2e/pyodide-node/tests/probe-export.test.mjs
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const DIST = resolve(__dirname, "../../../dist/pyodide-custom");
const SKIP = process.env.SKIP_PYODIDE === "1";

describe("Pyodide same-module probe", () => {
  it("custom distribution contains pyodide.asm.js", { skip: SKIP }, () => {
    assert.ok(
      existsSync(resolve(DIST, "pyodide.asm.js")),
      "pyodide.asm.js not found in " + DIST,
    );
  });

  it("exports wasmsh_probe_version from the main module", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createProbeModule } = await import("../host-wrapper.mjs");
    const mod = await createProbeModule();

    // Call the Rust probe function via Emscripten's ccall.
    // This proves same-module linking: the symbol lives inside
    // pyodide.asm.wasm, not in a separate side-loaded module.
    const version = mod.ccall("wasmsh_probe_version", "string", [], []);
    assert.equal(version, "0.1.0");
  });
});
