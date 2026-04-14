/**
 * Component-level Python execution contract tests for the wasmsh WASI P2
 * component artifact.
 *
 * These prove that the compiled component can execute `python` / `python3`
 * inside the component itself, through the exported `handle-json` resource,
 * under a real WASI P2 host (Wasmtime).
 *
 * The test scenarios mirror the Pyodide oracle in
 *   `e2e/pyodide-node/tests/python-command.test.mjs`
 * and will only pass once the component artifact contains an embedded Python
 * runtime wired through the same JSON bridge.
 *
 * Skip: SKIP_WASIP2=1 or SKIP_WASMTIME=1
 */
import { describe, it, before } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync } from "node:fs";
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
const HOST_RUNNER_DIR = resolve(REPO_ROOT, "tools/component-host-runner");
const HOST_RUNNER_BIN = resolve(
  HOST_RUNNER_DIR,
  "target/debug/component-host-runner",
);

const SKIP_WASIP2 = process.env.SKIP_WASIP2 === "1";
const SKIP_WASMTIME = process.env.SKIP_WASMTIME === "1";
const SKIP = SKIP_WASIP2 || SKIP_WASMTIME;

function commandExists(cmd) {
  return spawnSync("which", [cmd], { stdio: "ignore" }).status === 0;
}

// ── Helpers ──────────────────────────────────────────────────────

/** Decode a Stdout event's byte array into a UTF-8 string. */
function decodeStdout(events) {
  const chunks = events
    .filter((e) => "Stdout" in e)
    .map((e) => e.Stdout);
  if (chunks.length === 0) return null;
  const bytes = chunks.flat();
  return new TextDecoder().decode(new Uint8Array(bytes));
}

/** Decode a Stderr event's byte array into a UTF-8 string. */
function decodeStderr(events) {
  const chunks = events
    .filter((e) => "Stderr" in e)
    .map((e) => e.Stderr);
  if (chunks.length === 0) return null;
  const bytes = chunks.flat();
  return new TextDecoder().decode(new Uint8Array(bytes));
}

/** Extract exit code from events. */
function exitCode(events) {
  const exit = events.find((e) => "Exit" in e);
  return exit ? exit.Exit : null;
}

/**
 * Send a sequence of JSON commands through the component handle via the
 * host runner. Returns an array of parsed event arrays (one per command).
 */
function runComponent(workspaceDir, ...jsonCommands) {
  const result = spawnSync(
    HOST_RUNNER_BIN,
    [ARTIFACT, workspaceDir, ...jsonCommands.map((c) => JSON.stringify(c))],
    {
      encoding: "utf-8",
      timeout: 120_000,
    },
  );
  if (result.status !== 0) {
    throw new Error(
      `host-runner failed (exit ${result.status ?? "?"}):\n` +
        `stdout: ${result.stdout ?? ""}\n` +
        `stderr: ${result.stderr ?? ""}`,
    );
  }
  // Each line of stdout is a JSON array of WorkerEvents for one command.
  return result.stdout
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
}

// ── Test suite ───────────────────────────────────────────────────

describe("wasmsh-component Python execution contracts", () => {
  before(function () {
    if (SKIP) return;

    // Ensure the component artifact exists (build if needed).
    if (!existsSync(ARTIFACT)) {
      execFileSync("bash", [BUILD_SCRIPT], {
        cwd: REPO_ROOT,
        timeout: 600_000,
        stdio: ["ignore", "pipe", "pipe"],
      });
    }
    assert.ok(existsSync(ARTIFACT), `Component artifact missing: ${ARTIFACT}`);

    // Ensure the host runner binary exists (build if needed).
    if (!existsSync(HOST_RUNNER_BIN)) {
      execFileSync("cargo", ["build"], {
        cwd: HOST_RUNNER_DIR,
        timeout: 600_000,
        stdio: ["ignore", "pipe", "pipe"],
      });
    }
    assert.ok(
      existsSync(HOST_RUNNER_BIN),
      `Host runner binary missing: ${HOST_RUNNER_BIN}`,
    );
  });

  // ── python3 -c ──────────────────────────────────────────────

  it(
    "python3 -c 'print(40+2)' produces stdout 42",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [initEvents, runEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        { Run: { input: "python3 -c 'print(40+2)'" } },
      );

      const stdout = decodeStdout(runEvents);
      const stderr = decodeStderr(runEvents);
      assert.equal(
        stdout,
        "42\n",
        `Expected stdout "42\\n" but got: ${JSON.stringify(stdout)}\n` +
          `stderr: ${JSON.stringify(stderr)}`,
      );
      assert.equal(exitCode(runEvents), 0, `stderr: ${stderr}`);
    },
  );

  // ── heredoc-fed Python script ───────────────────────────────

  it(
    "python3 reads stdin from heredoc",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [, runEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        {
          Run: {
            input: `python3 <<'EOF'
import sys
print("hello from heredoc")
EOF`,
          },
        },
      );

      const stdout = decodeStdout(runEvents);
      assert.equal(
        stdout,
        "hello from heredoc\n",
        `Expected heredoc output but got: ${JSON.stringify(stdout)}`,
      );
      assert.equal(exitCode(runEvents), 0);
    },
  );

  // ── Python writes a file that shell reads ───────────────────

  it(
    "Python writes a file, shell reads it in the same handle",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [, runEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        {
          Run: {
            input:
              `python3 -c 'open("/workspace/from_py.txt","w").write("py-wrote-this")' && cat /workspace/from_py.txt`,
          },
        },
      );

      const stdout = decodeStdout(runEvents);
      assert.equal(
        stdout,
        "py-wrote-this",
        `Expected shared-FS read but got: ${JSON.stringify(stdout)}`,
      );
      assert.equal(exitCode(runEvents), 0);
    },
  );

  // ── Shell writes a file that Python reads ───────────────────

  it(
    "Shell writes a file, Python reads it in the same handle",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [, writeEvents, pyEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        {
          WriteFile: {
            path: "/workspace/from_shell.txt",
            data: Array.from(new TextEncoder().encode("shell-wrote-this")),
          },
        },
        {
          Run: {
            input: `python3 -c 'print(open("/workspace/from_shell.txt").read())'`,
          },
        },
      );

      const stdout = decodeStdout(pyEvents);
      assert.equal(
        stdout,
        "shell-wrote-this\n",
        `Expected Python to read shell-written file but got: ${JSON.stringify(stdout)}`,
      );
      assert.equal(exitCode(pyEvents), 0);
    },
  );

  // ── Python stderr surfaces as WorkerEvent::Stderr ───────────

  it(
    "python3 stderr surfaces in worker events",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [, runEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        {
          Run: {
            input:
              `python3 -c 'import sys; print("err-msg", file=sys.stderr)'`,
          },
        },
      );

      const stderr = decodeStderr(runEvents);
      assert.ok(
        stderr && stderr.includes("err-msg"),
        `Expected stderr to include "err-msg" but got: ${JSON.stringify(stderr)}`,
      );
      assert.equal(exitCode(runEvents), 0);
    },
  );

  // ── sqlite3 offline stdlib ──────────────────────────────────

  it(
    "python3 can use sqlite3 stdlib end-to-end",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const script = `python3 <<'EOF'
import sqlite3
conn = sqlite3.connect(":memory:")
cur = conn.cursor()
cur.execute("CREATE TABLE t (id INTEGER, name TEXT)")
cur.executemany("INSERT INTO t VALUES (?,?)", [(1,"a"),(2,"b"),(3,"c")])
conn.commit()
cur.execute("SELECT COUNT(*) FROM t")
print("count:", cur.fetchone()[0])
cur.execute("SELECT name FROM t ORDER BY id DESC LIMIT 1")
print("last:", cur.fetchone()[0])
conn.close()
EOF`;
      const [, runEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        { Run: { input: script } },
      );

      const stdout = decodeStdout(runEvents);
      const stderr = decodeStderr(runEvents);
      assert.ok(
        stdout?.includes("count: 3") && stdout?.includes("last: c"),
        `Expected sqlite3 output but got stdout: ${JSON.stringify(stdout)}\n` +
          `stderr: ${JSON.stringify(stderr)}`,
      );
      assert.equal(exitCode(runEvents), 0, `stderr: ${stderr}`);
    },
  );

  // ── Both python and python3 spellings accepted ──────────────

  it(
    "both 'python' and 'python3' spellings are accepted",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const [, py3Events, pyEvents] = runComponent(
        workspace,
        { Init: { step_budget: 100_000, allowed_hosts: [] } },
        { Run: { input: "python3 -c 'print(1)'" } },
        { Run: { input: "python -c 'print(2)'" } },
      );

      assert.equal(
        decodeStdout(py3Events),
        "1\n",
        `python3 spelling failed`,
      );
      assert.equal(
        decodeStdout(pyEvents),
        "2\n",
        `python spelling failed`,
      );
    },
  );
});
