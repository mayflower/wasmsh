/**
 * Build-contract test for the no-JS same-module Pyodide artifact intended for
 * Wasmtime.
 *
 * Verifies that:
 *   1. `bash tools/pyodide/build-wasi.sh` succeeds
 *   2. the expected artifact `dist/pyodide-wasi/wasmsh_pyodide_wasi.wasm`
 *      exists and is non-empty
 *   3. `dist/pyodide-wasi/manifest.json` exists and contains the required
 *      metadata fields
 *   4. when `wasm-tools` is available, the artifact exports the symbols needed
 *      for the Wasmtime host path
 *
 * Skip knobs:
 *   SKIP_PYODIDE_WASI=1   Skip everything in this file
 *   SKIP_WASMTOOLS=1           Skip only the export inspection
 */
import { describe, it } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const BUILD_SCRIPT = resolve(REPO_ROOT, "tools/pyodide/build-wasi.sh");
const ARTIFACT_DIR = resolve(REPO_ROOT, "dist/pyodide-wasi");
const ARTIFACT = resolve(ARTIFACT_DIR, "wasmsh_pyodide_wasi.wasm");
const MANIFEST = resolve(ARTIFACT_DIR, "manifest.json");

const SKIP = process.env.SKIP_PYODIDE_WASI === "1";
const SKIP_WASMTOOLS = process.env.SKIP_WASMTOOLS === "1";

function commandExists(cmd) {
  return spawnSync("which", [cmd], { stdio: "ignore" }).status === 0;
}

describe("pyodide-wasi build contract", () => {
  it(
    "build script produces the same-module artifact",
    { skip: SKIP, timeout: 900_000 },
    () => {
      assert.ok(
        existsSync(BUILD_SCRIPT),
        `Build script missing: ${BUILD_SCRIPT}`,
      );

      let output;
      try {
        output = execFileSync("bash", [BUILD_SCRIPT], {
          cwd: REPO_ROOT,
          encoding: "utf-8",
          timeout: 600_000,
          stdio: ["ignore", "pipe", "pipe"],
        });
      } catch (err) {
        const stdout = err.stdout?.toString?.() ?? "";
        const stderr = err.stderr?.toString?.() ?? "";
        throw new Error(
          `build-wasi.sh failed with exit ${err.status ?? "?"}:\n` +
            `--- stdout ---\n${stdout.slice(-2000)}\n` +
            `--- stderr ---\n${stderr.slice(-2000)}`,
        );
      }

      console.log(output);
      assert.ok(existsSync(ARTIFACT), `Artifact missing: ${ARTIFACT}`);

      const stat = statSync(ARTIFACT);
      assert.ok(stat.size > 0, `Artifact is empty (${stat.size} bytes)`);
      console.log(`Artifact size: ${stat.size} bytes`);
    },
  );

  it(
    "artifact is located in the expected output directory",
    { skip: SKIP },
    () => {
      assert.ok(
        ARTIFACT.includes("dist/pyodide-wasi"),
        "Artifact path should include dist/pyodide-wasi",
      );
      assert.ok(
        ARTIFACT.endsWith(".wasm"),
        "Artifact should have .wasm extension",
      );
    },
  );

  it(
    "manifest.json contains required metadata fields",
    { skip: SKIP, timeout: 30_000 },
    () => {
      assert.ok(existsSync(MANIFEST), `Manifest missing: ${MANIFEST}`);

      const raw = readFileSync(MANIFEST, "utf-8");
      let manifest;
      try {
        manifest = JSON.parse(raw);
      } catch {
        throw new Error(`manifest.json is not valid JSON:\n${raw.slice(0, 500)}`);
      }

      const requiredFields = [
        "artifact",
        "entryExport",
        "bootExport",
        "stdlibMode",
        "pyodideVersion",
        "emscriptenVersion",
      ];

      for (const field of requiredFields) {
        assert.ok(
          field in manifest,
          `manifest.json missing required field: "${field}"`,
        );
      }

      console.log("manifest.json:", JSON.stringify(manifest, null, 2));
    },
  );

  it(
    "artifact exports the symbols needed for the Wasmtime host path",
    {
      skip: SKIP || SKIP_WASMTOOLS || !commandExists("wasm-tools"),
      timeout: 60_000,
    },
    () => {
      assert.ok(existsSync(ARTIFACT), `Artifact missing: ${ARTIFACT}`);

      // Use wasm-tools to print the module's exports.
      const result = spawnSync(
        "wasm-tools",
        ["print", "--skeleton", ARTIFACT],
        {
          encoding: "utf-8",
          timeout: 60_000,
        },
      );

      if (result.status !== 0) {
        throw new Error(
          `wasm-tools print failed:\n${result.stderr ?? ""}`,
        );
      }

      const wat = result.stdout ?? "";

      // Extract all (export ...) lines.
      const exportLines = wat
        .split("\n")
        .filter((line) => line.includes("(export"));

      const exportNames = exportLines.map((line) => {
        const match = line.match(/\(export\s+"([^"]+)"/);
        return match ? match[1] : null;
      }).filter(Boolean);

      console.log("Exports found:", exportNames.join(", "));

      // These are the symbols the Wasmtime runner needs.
      // Emscripten strips the C-level `_` prefix from wasm exports, so
      // the exported names match the Rust `#[no_mangle]` identifiers directly.
      const requiredExports = [
        "malloc",
        "free",
        "wasmsh_pyodide_boot",
        "wasmsh_runtime_new",
        "wasmsh_runtime_handle_json",
        "wasmsh_runtime_free",
        "wasmsh_runtime_free_string",
      ];

      // With STANDALONE_WASM=1, Emscripten produces `_start` or `_initialize`.
      const hasEntry =
        exportNames.includes("_initialize") ||
        exportNames.includes("_start");
      assert.ok(
        hasEntry,
        `Artifact must export either "_initialize" or "_start".\n` +
          `Found exports: ${exportNames.join(", ")}`,
      );

      // Memory must be exported (not imported) for the standalone path.
      assert.ok(
        exportNames.includes("memory"),
        `Artifact must export "memory".\n` +
          `Found exports: ${exportNames.join(", ")}`,
      );

      for (const name of requiredExports) {
        assert.ok(
          exportNames.includes(name),
          `Missing required export: "${name}".\n` +
            `Found exports: ${exportNames.join(", ")}`,
        );
      }
    },
  );
});
