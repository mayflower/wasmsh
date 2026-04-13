/**
 * Wasmtime-backed behavioral contracts for Python execution in the
 * standalone Pyodide+wasmsh artifact.
 *
 * These prove that the no-JS same-module artifact can run `python3`
 * through the existing JSON runtime API under Wasmtime, with shared
 * filesystem semantics matching the Pyodide Node oracle tests.
 *
 * The test scenarios mirror:
 *   e2e/pyodide-node/tests/python-command.test.mjs
 *   e2e/pyodide-node/tests/workspace-shell-to-python.test.mjs
 *   e2e/pyodide-node/tests/workspace-python-to-shell.test.mjs
 *
 * Skip knobs:
 *   SKIP_PYODIDE_WASI=1   Skip everything
 *   SKIP_WASMTIME=1            Skip Wasmtime execution only
 */
import { describe, it, before } from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
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

const SKIP_ALL = process.env.SKIP_PYODIDE_WASI === "1";
const SKIP_WASMTIME = process.env.SKIP_WASMTIME === "1";
const SKIP = SKIP_ALL || SKIP_WASMTIME;

// ── Helpers ──────────────────────────────────────────────────

/**
 * Run a scenario through the Wasmtime runner.
 * Returns the parsed result JSON.
 */
function runScenario(scenario) {
  const input = JSON.stringify(scenario);
  const result = spawnSync(RUNNER_BIN, [], {
    input,
    encoding: "utf-8",
    timeout: 120_000,
  });

  // The runner always outputs JSON to stdout, even on failure.
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
 * Build a standard scenario with boot + init + one or more commands.
 */
function makeScenario(workspaceDir, ...commands) {
  const steps = [
    { kind: "boot" },
    { kind: "init", step_budget: 100_000, allowed_hosts: [] },
  ];
  for (const cmd of commands) {
    if (typeof cmd === "string") {
      steps.push({ kind: "host-command", command: { Run: { input: cmd } } });
    } else {
      steps.push({ kind: "host-command", command: cmd });
    }
  }
  return {
    artifact: ARTIFACT,
    workspaceDir: workspaceDir,
    steps,
  };
}

/** Find the step result for the Nth host-command (0-based). */
function hostStep(result, n) {
  const cmds = result.steps.filter((s) => s.kind === "host-command");
  return cmds[n] ?? null;
}

// ── Setup ────────────────────────────────────────────────────

describe("pyodide-wasi Python behavioral contracts", () => {
  before(function () {
    if (SKIP) return;

    // Build the runner if needed.
    if (!existsSync(RUNNER_BIN)) {
      console.log("Building pyodide-wasi-host-runner...");
      try {
        execFileSync("cargo", ["build"], {
          cwd: RUNNER_DIR,
          timeout: 600_000,
          stdio: ["ignore", "pipe", "pipe"],
        });
      } catch (err) {
        const stderr = err.stderr?.toString?.() ?? "";
        throw new Error(
          `Failed to build pyodide-wasi-host-runner:\n${stderr.slice(-2000)}`,
        );
      }
    }

    // Verify the artifact exists.
    assert.ok(
      existsSync(ARTIFACT),
      `Artifact missing: ${ARTIFACT}\nRun: just build-pyodide-wasi`,
    );
  });

  // ── python3 -c ─────────────────────────────────────────────

  it(
    "python3 -c 'print(40+2)' produces stdout 42",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const result = runScenario(
        makeScenario(workspace, "python3 -c 'print(40+2)'"),
      );

      const step = hostStep(result, 0);
      assert.ok(step, `No host-command result.\nFull: ${JSON.stringify(result)}`);
      assert.equal(
        step.stdout,
        "42\n",
        `Expected stdout "42\\n" but got: ${JSON.stringify(step.stdout)}\n` +
          `stderr: ${JSON.stringify(step.stderr)}\n` +
          `[GAP] The artifact may not have a working Python runtime yet. ` +
          `If python3 returns "command not found", the boot path needs ` +
          `to initialize CPython before the JSON runtime processes commands.`,
      );
      assert.equal(step.exitCode, 0, `Unexpected exit: stderr=${step.stderr}`);
    },
  );

  // ── heredoc-fed Python script ──────────────────────────────

  it(
    "python3 reads stdin from heredoc",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const result = runScenario(
        makeScenario(
          workspace,
          `python3 <<'EOF'\nimport sys\nprint("hello from heredoc")\nEOF`,
        ),
      );

      const step = hostStep(result, 0);
      assert.ok(step, "No host-command result");
      assert.equal(
        step.stdout,
        "hello from heredoc\n",
        `Expected heredoc output but got: ${JSON.stringify(step.stdout)}\n` +
          `[GAP] Requires Python runtime + heredoc/stdin pipe integration.`,
      );
      assert.equal(step.exitCode, 0);
    },
  );

  // ── Shell writes file, Python reads it ─────────────────────

  it(
    "shell writes a file, Python reads it in the same session",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const result = runScenario(
        makeScenario(
          workspace,
          `echo -n "shell-wrote-this" > /workspace/from_shell.txt`,
          `python3 -c 'print(open("/workspace/from_shell.txt").read())'`,
        ),
      );

      const pyStep = hostStep(result, 1);
      assert.ok(pyStep, "No Python step result");
      assert.equal(
        pyStep.stdout,
        "shell-wrote-this\n",
        `Expected Python to read shell-written file but got: ${JSON.stringify(pyStep.stdout)}\n` +
          `[GAP] Requires shared filesystem between shell and Python.`,
      );
      assert.equal(pyStep.exitCode, 0);
    },
  );

  // ── Python writes file, shell reads it ─────────────────────

  it(
    "Python writes a file, shell reads it in the same session",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const result = runScenario(
        makeScenario(
          workspace,
          `python3 -c 'open("/workspace/from_py.txt","w").write("py-wrote-this")'`,
          `cat /workspace/from_py.txt`,
        ),
      );

      const catStep = hostStep(result, 1);
      assert.ok(catStep, "No cat step result");
      assert.equal(
        catStep.stdout,
        "py-wrote-this",
        `Expected shell to read Python-written file but got: ${JSON.stringify(catStep.stdout)}\n` +
          `[GAP] Requires shared filesystem between Python and shell.`,
      );
      assert.equal(catStep.exitCode, 0);
    },
  );

  // ── Python stderr surfaces in events ───────────────────────

  it(
    "python3 stderr surfaces in worker events",
    { skip: SKIP, timeout: 120_000 },
    () => {
      const workspace = mkdtempSync(resolve(os.tmpdir(), "wasmsh-py-"));
      const result = runScenario(
        makeScenario(
          workspace,
          `python3 -c 'import sys; print("err-msg", file=sys.stderr)'`,
        ),
      );

      const step = hostStep(result, 0);
      assert.ok(step, "No host-command result");
      assert.ok(
        step.stderr && step.stderr.includes("err-msg"),
        `Expected stderr to include "err-msg" but got: ${JSON.stringify(step.stderr)}\n` +
          `[GAP] Requires Python stderr capture and propagation.`,
      );
      assert.equal(step.exitCode, 0);
    },
  );
});
