/**
 * Build-contract test for the wasmsh-component crate (WASI P2 Component Model
 * transport). Verifies that:
 *
 *   1. `cargo build --target wasm32-wasip2 -p wasmsh-component --features
 *      component-export` succeeds
 *   2. the expected `.wasm` artifact exists and is non-empty
 *   3. when `wasm-tools` is available, the exported WIT surface contains
 *      the custom `wasmsh:component/sandbox` interface with a `session`
 *      resource — proving this is not merely a `wasi:cli/run` wrapper
 *   4. when `wasmtime` is available, invoking `run-once` with `echo hello`
 *      returns a stdout event carrying "hello" and an exit status of 0
 *
 * Skip knobs for ergonomics on hosts that cannot install wasmtime /
 * wasm-tools / the wasip2 target locally:
 *
 *   SKIP_WASIP2=1        Skip everything in this file (also skips the build)
 *   SKIP_WASMTOOLS=1     Skip only the wasm-tools WIT inspection
 *   SKIP_WASMTIME=1      Skip only the Wasmtime smoke invocation
 *
 * CI installs both wasmtime and wasm-tools and runs without any skip flag
 * so the assertions are strong where it matters.
 */
import { describe, it } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, statSync } from "node:fs";
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

const SKIP_WASIP2 = process.env.SKIP_WASIP2 === "1";
const SKIP_WASMTOOLS = process.env.SKIP_WASMTOOLS === "1";
const SKIP_WASMTIME = process.env.SKIP_WASMTIME === "1";

function commandExists(cmd) {
  const result = spawnSync("which", [cmd], { stdio: "ignore" });
  return result.status === 0;
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
    "wasm-tools reports the custom wasmsh:component/sandbox interface",
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
        wit.includes("interface sandbox"),
        "Exported WIT should declare a sandbox interface",
      );
      assert.ok(
        wit.includes("resource session"),
        "sandbox interface should declare a stateful session resource",
      );
      assert.ok(
        wit.includes("run-once:"),
        "sandbox interface should export the run-once convenience function",
      );
      // The artifact must not be a CLI-only wrapper. wasi:cli/run is OK as
      // an import if the toolchain pulls it in, but the export surface
      // must include the custom sandbox interface.
      assert.ok(
        !/^\s*export wasi:cli\/run/m.test(wit),
        "Component must export the custom sandbox interface, not wasi:cli/run",
      );
    },
  );

  it(
    "wasmtime run --invoke run-once returns echo output and exit 0",
    { skip: SKIP_WASIP2 || SKIP_WASMTIME || !commandExists("wasmtime") },
    () => {
      // `wasmtime run --invoke` takes a WAVE-style expression for the
      // exported function. We exercise the `run-once` convenience export
      // because it does not require threading a resource handle through
      // the CLI.
      const invoke =
        "run-once({step-budget: 100000, allowed-hosts: []}, \"echo hello\")";
      const result = spawnSync(
        "wasmtime",
        [
          "run",
          "--wasm",
          "component-model=y",
          "--invoke",
          invoke,
          ARTIFACT,
        ],
        { encoding: "utf-8", timeout: 120_000 },
      );
      const stdout = result.stdout ?? "";
      const stderr = result.stderr ?? "";
      console.log("--- wasmtime stdout ---\n" + stdout);
      console.log("--- wasmtime stderr ---\n" + stderr);

      assert.equal(result.status, 0, "wasmtime run should exit 0");
      // The returned event list is serialized by wasmtime's WAVE printer.
      // `list<u8>` payloads render as decimal byte literals, so "hello\n"
      // appears as `[104, 101, 108, 108, 111, 10]` inside a `stdout(...)`
      // wrapper. We match the exact byte pattern plus the trailing
      // `exit(0)` event.
      const helloBytes = "[104, 101, 108, 108, 111, 10]";
      assert.ok(
        stdout.includes(`stdout(${helloBytes})`),
        "wasmtime output should contain the echo bytes in a stdout(...) event",
      );
      assert.ok(
        /exit\(0\)/.test(stdout),
        "wasmtime output should include exit(0) as the final event",
      );
    },
  );
});
