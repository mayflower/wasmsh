/**
 * Build-contract test for the wasmsh-component crate (WASI P2 Component Model
 * transport). Verifies that:
 *
 *   1. `cargo build --target wasm32-wasip2 -p wasmsh-component --features
 *      component-export` succeeds
 *   2. the expected `.wasm` artifact exists and is non-empty
 *   3. when `wasm-tools` is available, the exported WIT surface is the thin
 *      Pyodide-parity transport:
 *        - `interface runtime`
 *        - `resource handle`
 *        - `handle-json`
 *        - `probe-version`
 *        - `probe-write-text`
 *        - `probe-file-equals`
 *      and does not expose the old bespoke typed sandbox/session API
 *
 * Skip knobs for ergonomics on hosts that cannot install wasm-tools / the
 * wasip2 target locally:
 *
 *   SKIP_WASIP2=1        Skip everything in this file (also skips the build)
 *   SKIP_WASMTOOLS=1     Skip only the wasm-tools WIT inspection
 */
import { describe, it } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, statSync } from "node:fs";
import os from "node:os";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const BUILD_SCRIPT = resolve(__dirname, "../build-component.sh");
const ARTIFACT = resolve(
  REPO_ROOT,
  "target/wasm32-wasip2/debug/wasmsh_component.wasm",
);
const PROTOCOL_LIB = resolve(
  REPO_ROOT,
  "crates/wasmsh-protocol/src/lib.rs",
);

const SKIP_WASIP2 = process.env.SKIP_WASIP2 === "1";
const SKIP_WASMTOOLS = process.env.SKIP_WASMTOOLS === "1";
const SKIP_WASMTIME = process.env.SKIP_WASMTIME === "1";
function commandExists(cmd) {
  const result = spawnSync("which", [cmd], { stdio: "ignore" });
  return result.status === 0;
}

function protocolVersion() {
  const content = readFileSync(PROTOCOL_LIB, "utf-8");
  const match = content.match(/PROTOCOL_VERSION:\s*&str\s*=\s*"([^"]+)"/);
  assert.ok(match, "failed to read PROTOCOL_VERSION from wasmsh-protocol");
  return match[1];
}

function invokeComponent(call, hostWorkspaceDir) {
  const result = spawnSync(
    "wasmtime",
    [
      "run",
      "--dir",
      `${hostWorkspaceDir}::/workspace`,
      "--invoke",
      call,
      ARTIFACT,
    ],
    {
      encoding: "utf-8",
      timeout: 120_000,
    },
  );
  if (result.status === 0) {
    return {
      stdout: result.stdout ?? "",
      stderr: result.stderr ?? "",
    };
  }
  throw new Error(
    `wasmtime invoke failed for ${call} with exit ${result.status ?? "?"}\n` +
      `stdout:\n${result.stdout ?? ""}\n` +
      `stderr:\n${result.stderr ?? ""}`,
  );
}

describe("wasmsh-component wasm32-wasip2 build", () => {
  it("build script produces the component artifact", { skip: SKIP_WASIP2 }, () => {
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
        `build-component.sh failed with exit ${err.status ?? "?"}:\n` +
          `--- stdout ---\n${stdout}\n` +
          `--- stderr ---\n${stderr}`,
      );
    }

    console.log(output);
    assert.ok(existsSync(ARTIFACT), "Expected artifact at " + ARTIFACT);

    const stat = statSync(ARTIFACT);
    assert.ok(stat.size > 0, "Artifact is empty (" + stat.size + " bytes)");
  });

  it("artifact path follows wasip2 target convention", { skip: SKIP_WASIP2 }, () => {
    assert.ok(
      ARTIFACT.includes("wasm32-wasip2"),
      "Artifact path should contain the wasip2 target triple",
    );
    assert.ok(
      ARTIFACT.endsWith(".wasm"),
      "Component artifact should have .wasm extension",
    );
  });

  it(
    "wasm-tools reports the thin Pyodide-parity runtime interface",
    { skip: SKIP_WASIP2 || SKIP_WASMTOOLS || !commandExists("wasm-tools") },
    () => {
      const result = spawnSync("wasm-tools", ["component", "wit", ARTIFACT], {
        encoding: "utf-8",
        timeout: 120_000,
      });
      if (result.status !== 0) {
        throw new Error(
          `wasm-tools component wit failed:\n${result.stderr ?? ""}`,
        );
      }
      const wit = result.stdout ?? "";
      console.log(wit);

      assert.ok(
        wit.includes("package wasmsh:component"),
        "Exported WIT should declare the wasmsh:component package",
      );
      assert.ok(
        wit.includes("interface runtime"),
        "Exported WIT should declare a runtime interface",
      );
      assert.ok(
        wit.includes("resource handle"),
        "runtime interface should declare a handle resource",
      );
      assert.ok(
        wit.includes("handle-json:"),
        "handle resource should export handle-json",
      );
      assert.ok(
        wit.includes("probe-version:"),
        "runtime interface should export probe-version",
      );
      assert.ok(
        wit.includes("probe-write-text:"),
        "runtime interface should export probe-write-text",
      );
      assert.ok(
        wit.includes("probe-file-equals:"),
        "runtime interface should export probe-file-equals",
      );
      assert.ok(
        !wit.includes("resource session"),
        "runtime interface must not retain the old session resource",
      );
      assert.ok(
        !wit.includes("run-once:"),
        "runtime interface must not retain the run-once helper",
      );
      assert.ok(
        !wit.includes("write-file:"),
        "runtime interface must not export a typed write-file helper",
      );
      assert.ok(
        !wit.includes("read-file:"),
        "runtime interface must not export a typed read-file helper",
      );
      assert.ok(
        !wit.includes("list-dir:"),
        "runtime interface must not export a typed list-dir helper",
      );
      assert.ok(
        !wit.includes("mount:"),
        "runtime interface must not export a typed mount helper",
      );
      // The artifact must not be a CLI-only wrapper. wasi:cli/run is OK as
      // an import if the toolchain pulls it in, but the export surface
      // must include the custom runtime interface.
      assert.ok(
        !/^\s*export wasi:cli\/run/m.test(wit),
        "Component must export the custom runtime interface, not wasi:cli/run",
      );
    },
  );

  it(
    "wasmtime probe-version reports the protocol version",
    { skip: SKIP_WASIP2 || SKIP_WASMTIME || !commandExists("wasmtime") },
    () => {
      const hostWorkspaceDir = mkdtempSync(
        resolve(os.tmpdir(), "wasmsh-component-contract-"),
      );
      const result = invokeComponent("probe-version()", hostWorkspaceDir);
      assert.equal(
        JSON.parse(result.stdout.trim()),
        protocolVersion(),
        `expected protocol version, got ${result.stdout}`,
      );
    },
  );

  it(
    "wasmtime probe helpers perform real file I/O through /workspace",
    { skip: SKIP_WASIP2 || SKIP_WASMTIME || !commandExists("wasmtime") },
    () => {
      const hostWorkspaceDir = mkdtempSync(
        resolve(os.tmpdir(), "wasmsh-component-contract-"),
      );
      const write = invokeComponent(
        'probe-write-text("/workspace/contract.txt", "hello")',
        hostWorkspaceDir,
      );
      assert.match(
        write.stdout.trim(),
        /^(0|\(0\))$/,
        `expected probe write success, got ${write.stdout}`,
      );

      const equals = invokeComponent(
        'probe-file-equals("/workspace/contract.txt", "hello")',
        hostWorkspaceDir,
      );
      assert.match(
        equals.stdout.trim(),
        /^(true|1|\(true\)|\(1\))$/,
        `expected probe file-equals success, got ${equals.stdout}`,
      );
    },
  );
});
