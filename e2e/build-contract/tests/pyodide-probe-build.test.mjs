/**
 * Build-contract test for the wasmsh-pyodide-probe crate.
 *
 * Verifies that `cargo build --target wasm32-unknown-emscripten` succeeds
 * and produces the expected staticlib artifact.
 *
 * Skip when emcc is not installed:
 *   SKIP_EMSCRIPTEN=1 node --test e2e/build-contract/tests/pyodide-probe-build.test.mjs
 */
import { describe, it } from "node:test";
import { execFileSync } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const BUILD_SCRIPT = resolve(__dirname, "../build-probe.sh");
const ARTIFACT = resolve(
  REPO_ROOT,
  "crates/wasmsh-pyodide-probe/target/wasm32-unknown-emscripten/release/libwasmsh_pyodide_probe.a",
);

const SKIP = process.env.SKIP_EMSCRIPTEN === "1";

describe("wasmsh-pyodide-probe emscripten build", () => {
  it("build script produces the staticlib artifact", { skip: SKIP }, () => {
    const output = execFileSync("bash", [BUILD_SCRIPT], {
      cwd: REPO_ROOT,
      encoding: "utf-8",
      timeout: 300_000,
      stdio: ["ignore", "pipe", "pipe"],
    });

    console.log(output);

    assert.ok(existsSync(ARTIFACT), "Expected artifact at " + ARTIFACT);

    const stat = statSync(ARTIFACT);
    assert.ok(stat.size > 0, "Artifact is empty (" + stat.size + " bytes)");
  });

  it("artifact path follows emscripten target convention", { skip: SKIP }, () => {
    assert.ok(
      ARTIFACT.includes("wasm32-unknown-emscripten"),
      "Artifact path should contain the emscripten target triple",
    );
    assert.ok(
      ARTIFACT.endsWith(".a"),
      "staticlib artifact should have .a extension",
    );
  });
});
