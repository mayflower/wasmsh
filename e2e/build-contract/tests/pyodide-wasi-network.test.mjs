/**
 * Wasmtime-backed network + micropip contracts for the standalone
 * Pyodide-WASI artifact.
 *
 * These mirror the behavioral oracles in:
 *   e2e/pyodide-node/tests/micropip-install.test.mjs
 *   e2e/pyodide-node/tests/network-security.test.mjs
 *
 * All tests use a local HTTP server — no live PyPI dependency.
 * The wheel fixture is already committed at e2e/fixtures/.
 *
 * Skip knobs:
 *   SKIP_PYODIDE_WASI=1   Skip everything
 *   SKIP_WASMTIME=1        Skip Wasmtime execution only
 */
import http from "node:http";
import { describe, it, before, after } from "node:test";
import { execFileSync, spawnSync, spawn } from "node:child_process";
import { existsSync, readFileSync, mkdtempSync } from "node:fs";
import os from "node:os";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, "../../..");
const ARTIFACT = resolve(
  REPO_ROOT,
  "dist/pyodide-wasi/wasmsh_pyodide_wasi.wasm",
);
const RUNNER_DIR = resolve(REPO_ROOT, "tools/pyodide-wasi-host-runner");
const RUNNER_BIN = resolve(RUNNER_DIR, "target/debug/pyodide-wasi-host-runner");
const WHEEL_PATH = resolve(
  REPO_ROOT,
  "e2e/fixtures/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
);

const SKIP_ALL = process.env.SKIP_PYODIDE_WASI === "1";
const SKIP_WASMTIME = process.env.SKIP_WASMTIME === "1";
const SKIP = SKIP_ALL || SKIP_WASMTIME;

/** Base64-encoded wheel fixture, written to memfs via Python. */
const WHEEL_B64 = readFileSync(WHEEL_PATH).toString("base64");

/** Python snippet to write wheel fixture to /tmp/ via base64 decode. */
const WRITE_WHEEL_CMD =
  `python3 -c 'import base64; open("/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl","wb").write(base64.b64decode("${WHEEL_B64}"))'`;



// ── Helpers ──────────────────────────────────────────────────

function runScenario(scenario) {
  const input = JSON.stringify(scenario);
  const result = spawnSync(RUNNER_BIN, [], {
    input,
    encoding: "utf-8",
    timeout: 120_000,
  });
  const stdout = (result.stdout ?? "").trim();
  if (!stdout) {
    throw new Error(
      `Runner produced no output.\n` +
        `exit: ${result.status}\n` +
        `stderr: ${result.stderr ?? ""}`,
    );
  }
  try {
    return JSON.parse(stdout);
  } catch {
    throw new Error(
      `Runner output is not valid JSON:\n${stdout}\n` +
        `stderr: ${result.stderr ?? ""}`,
    );
  }
}

/**
 * Async version of runScenario — doesn't block the event loop, allowing
 * the local HTTP server to handle requests during execution.
 */
function runScenarioAsync(scenario) {
  return new Promise((resolve, reject) => {
    const child = spawn(RUNNER_BIN, [], { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    child.on("close", () => {
      const trimmed = stdout.trim();
      if (!trimmed) {
        return reject(
          new Error(`Runner produced no output.\nstderr: ${stderr}`),
        );
      }
      try {
        resolve(JSON.parse(trimmed));
      } catch {
        reject(
          new Error(
            `Runner output is not valid JSON:\n${trimmed}\nstderr: ${stderr}`,
          ),
        );
      }
    });
    child.on("error", reject);
    child.stdin.write(JSON.stringify(scenario));
    child.stdin.end();
    setTimeout(() => {
      child.kill();
      reject(new Error("Runner timed out after 120s"));
    }, 120_000);
  });
}

/**
 * Build a scenario with boot + init (custom allowed_hosts) + commands.
 */
function makeScenario(workspaceDir, allowedHosts, ...commands) {
  const steps = [
    { kind: "boot" },
    { kind: "init", step_budget: 100_000, allowed_hosts: allowedHosts },
  ];
  for (const cmd of commands) {
    if (typeof cmd === "string") {
      steps.push({ kind: "host-command", command: { Run: { input: cmd } } });
    } else {
      steps.push({ kind: "host-command", command: cmd });
    }
  }
  return { artifact: ARTIFACT, workspaceDir, steps };
}

function hostStep(result, n) {
  const cmds = result.steps.filter((s) => s.kind === "host-command");
  return cmds[n] ?? null;
}

// ── Local HTTP server ───────────────────────────────────────

let server;
let serverPort;

// ── Setup ────────────────────────────────────────────────────

describe("pyodide-wasi network + micropip contracts", () => {
  before(async function () {
    if (SKIP) return;

    // Build the runner if needed.
    if (!existsSync(RUNNER_BIN)) {
      execFileSync("cargo", ["build"], {
        cwd: RUNNER_DIR,
        timeout: 600_000,
        stdio: ["ignore", "pipe", "pipe"],
      });
    }

    assert.ok(existsSync(ARTIFACT), `Artifact missing: ${ARTIFACT}`);
    assert.ok(existsSync(WHEEL_PATH), `Wheel fixture missing: ${WHEEL_PATH}`);

    // Start local HTTP server serving the wheel fixture.
    const wheelBytes = readFileSync(WHEEL_PATH);
    server = http.createServer((req, res) => {
      if (req.url?.endsWith(".whl")) {
        res.writeHead(200, {
          "Content-Type": "application/octet-stream",
          "Content-Length": wheelBytes.length,
        });
        res.end(wheelBytes);
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });
    await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
    serverPort = server.address().port;
  });

  after(() => {
    if (server) server.close();
  });

  // ── 1. Offline emfs: wheel install ────────────────────────

  it(
    "offline emfs: wheel install succeeds with empty allowed_hosts",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-net-"));
      // Upload wheel to memfs, then install via micropip
      const result = runScenario(
        makeScenario(
          workspace,
          [], // no allowed hosts
          // Write wheel fixture to memfs via base64 decode
          WRITE_WHEEL_CMD,
          // Install via micropip from emfs: URL using sync helper
          `python3 << 'PYEOF'
import _emfs_handler; _emfs_handler.install()
import _micropip_sync
_micropip_sync.install("emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl")
from wasmsh_test_fixture import GREETING
print(GREETING)
PYEOF`,
        ),
      );
      const step = hostStep(result, 1);
      assert.ok(step, "No micropip install result");
      assert.ok(
        step.stdout.includes("hello from wasmsh_test_fixture"),
        `Expected greeting but got: ${JSON.stringify(step.stdout)}\n` +
          `stderr: ${JSON.stringify(step.stderr)}\n` +
          `[GAP] Requires micropip embedded in the standalone artifact.`,
      );
      assert.equal(step.exitCode, 0);
    },
  );

  // ── 2. HTTP install denied by default ─────────────────────

  it(
    "HTTP wheel install denied with empty allowed_hosts",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-net-"));
      const url = `http://127.0.0.1:${serverPort}/wasmsh_test_fixture-0.1.0-py3-none-any.whl`;
      const result = runScenario(
        makeScenario(
          workspace,
          [], // empty allowlist
          `python3 << 'PYEOF'
import _micropip_sync
_micropip_sync.install("${url}")
PYEOF`,
        ),
      );
      const step = hostStep(result, 0);
      assert.ok(step, "No result");
      assert.notEqual(
        step.exitCode,
        0,
        `HTTP install should fail with empty allowlist.\n` +
          `stdout: ${JSON.stringify(step.stdout)}\n` +
          `[GAP] Requires network backend + allowlist enforcement.`,
      );
    },
  );

  // ── 3. HTTP install allowed for matching host ─────────────

  it(
    "HTTP wheel install succeeds with matching allowed_hosts",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-net-"));
      const url = `http://127.0.0.1:${serverPort}/wasmsh_test_fixture-0.1.0-py3-none-any.whl`;
      const result = await runScenarioAsync(
        makeScenario(
          workspace,
          [`127.0.0.1:${serverPort}`], // matching host
          `python3 << 'PYEOF'
import _micropip_sync
_micropip_sync.install("${url}")
from wasmsh_test_fixture import GREETING
print(GREETING)
PYEOF`,
        ),
      );
      const step = hostStep(result, 0);
      assert.ok(step, "No result");
      assert.ok(
        step.stdout.includes("hello from wasmsh_test_fixture"),
        `Expected greeting but got: ${JSON.stringify(step.stdout)}\n` +
          `stderr: ${JSON.stringify(step.stderr)}\n` +
          `[GAP] Requires working HTTP fetch in the standalone runtime.`,
      );
      assert.equal(step.exitCode, 0);
    },
  );

  // ── 4. HTTP install denied for non-matching host ──────────

  it(
    "HTTP wheel install denied for non-matching host",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-net-"));
      const url = `http://127.0.0.1:${serverPort}/wasmsh_test_fixture-0.1.0-py3-none-any.whl`;
      const result = runScenario(
        makeScenario(
          workspace,
          ["pypi.org"], // non-matching
          `python3 << 'PYEOF'
import _micropip_sync
_micropip_sync.install("${url}")
PYEOF`,
        ),
      );
      const step = hostStep(result, 0);
      assert.ok(step, "No result");
      assert.notEqual(
        step.exitCode,
        0,
        `HTTP install should fail with non-matching allowlist.\n` +
          `[GAP] Requires allowlist enforcement in the standalone network backend.`,
      );
    },
  );

  // ── 5. Shell HTTP utilities share the same backend ────────

  it(
    "curl to allowed host succeeds, denied host fails",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-net-"));
      const url = `http://127.0.0.1:${serverPort}/test.whl`;
      const result = await runScenarioAsync(
        makeScenario(
          workspace,
          [`127.0.0.1:${serverPort}`],
          // Allowed: curl to the local test server
          `curl ${url}`,
          // Denied: curl to a host not in the allowlist
          `curl http://evil.example.com/test`,
        ),
      );
      const allowedStep = hostStep(result, 0);
      assert.ok(allowedStep, "No curl-allowed result");
      assert.equal(
        allowedStep.exitCode,
        0,
        `curl to allowed host should succeed.\n` +
          `stdout: ${JSON.stringify(allowedStep.stdout)}\n` +
          `stderr: ${JSON.stringify(allowedStep.stderr)}\n` +
          `[GAP] Requires curl using the standalone network backend.`,
      );

      const deniedStep = hostStep(result, 1);
      assert.ok(deniedStep, "No curl-denied result");
      assert.notEqual(
        deniedStep.exitCode,
        0,
        `curl to non-allowed host should fail.\n` +
          `[GAP] Requires allowlist enforcement for shell HTTP utilities.`,
      );
    },
  );
});
