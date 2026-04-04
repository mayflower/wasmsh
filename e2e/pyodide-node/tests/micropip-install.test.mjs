/**
 * Tests for micropip-based package installation in the Pyodide sandbox.
 *
 * Uses a committed wheel fixture (e2e/fixtures/wasmsh_test_fixture-*.whl)
 * to avoid hitting public PyPI. The wheel is written to the in-process
 * EmscriptenFs and installed via `emfs:` URL.
 *
 * Skip: SKIP_PYODIDE=1
 */
import http from "node:http";
import { describe, it } from "node:test";
import { existsSync, readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { createSessionTracker } from "./test-session-helper.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");
const FIXTURES_DIR = resolve(__dirname, "../../fixtures");

const WHEEL_PATH = resolve(
  FIXTURES_DIR,
  "wasmsh_test_fixture-0.1.0-py3-none-any.whl",
);

const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

describe("installPythonPackages (Node)", () => {
  const openSession = SKIP ? null : createSessionTracker(createNodeSession, ASSETS_DIR);

  // ── API existence ────────────────────────────────────────────

  it(
    "session exposes installPythonPackages method",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const session = await openSession();
      assert.equal(typeof session.installPythonPackages, "function");
    },
  );

  // ── emfs: wheel install ──────────────────────────────────────

  it(
    "installs a local wheel from emfs: and imports it",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession();

      // Upload the wheel fixture to the sandbox filesystem
      const wheelBytes = readFileSync(WHEEL_PATH);
      await session.writeFile(
        "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
        wheelBytes,
      );

      // Install from emfs: URL
      const result = await session.installPythonPackages(
        "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
      );
      assert.ok(result, "installPythonPackages should return a result");

      // Verify the package is importable
      const run = await session.run(
        "python3 -c \"from wasmsh_test_fixture import GREETING; print(GREETING)\"",
      );
      assert.equal(run.exitCode, 0, `python3 failed: ${run.stderr}`);
      assert.ok(
        run.stdout.includes("hello from wasmsh_test_fixture"),
        `unexpected stdout: ${run.stdout}`,
      );
    },
  );

  // ── string array requirements ────────────────────────────────

  it(
    "accepts an array of requirements",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession();
      const wheelBytes = readFileSync(WHEEL_PATH);
      await session.writeFile(
        "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
        wheelBytes,
      );

      const result = await session.installPythonPackages([
        "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
      ]);
      assert.ok(result, "installPythonPackages should return a result");
    },
  );

  // ── install result structure ─────────────────────────────────

  it(
    "returns structured install result",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession();
      const wheelBytes = readFileSync(WHEEL_PATH);
      await session.writeFile(
        "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
        wheelBytes,
      );

      const result = await session.installPythonPackages(
        "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
      );
      // Result should have at minimum: requested requirements + diagnostics
      assert.equal(typeof result, "object");
      assert.ok(
        "installed" in result || "requirements" in result,
        `result should contain installed or requirements: ${JSON.stringify(result)}`,
      );
    },
  );

  // ── Security: deny-by-default ─────────────────────────────────

  it(
    "rejects package name when no allowedHosts configured",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession({ allowedHosts: [] });
      await assert.rejects(
        () => session.installPythonPackages("requests"),
        /require.*network|not.*supported/i,
        "should reject package name installs without allowedHosts",
      );
    },
  );

  it(
    "rejects HTTP URL when host is not in allowlist",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession({ allowedHosts: [] });
      await assert.rejects(
        () =>
          session.installPythonPackages(
            "https://evil.example.com/malicious-1.0-py3-none-any.whl",
          ),
        /not allowed/i,
        "should reject HTTP URL with no allowedHosts",
      );
    },
  );

  it(
    "rejects HTTP URL when host does not match allowlist",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession({
        allowedHosts: ["pypi.org", "files.pythonhosted.org"],
      });
      await assert.rejects(
        () =>
          session.installPythonPackages(
            "https://evil.example.com/trojan-1.0-py3-none-any.whl",
          ),
        /not allowed/i,
        "should reject HTTP URL for non-allowlisted host",
      );
    },
  );

  it(
    "rejects file: URIs for security",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession();
      await assert.rejects(
        () => session.installPythonPackages("file:///etc/passwd"),
        /file.*not supported|security/i,
        "should reject file: URIs",
      );
    },
  );

  it(
    "rejects FILE: URIs case-insensitively",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession();
      await assert.rejects(
        () => session.installPythonPackages("FILE:///etc/shadow"),
        /file.*not supported|security/i,
        "should reject FILE: URIs (case-insensitive)",
      );
    },
  );

  // ── emfs: still works alongside security checks ──────────────

  it(
    "emfs: installs work even with empty allowedHosts",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      const session = await openSession({ allowedHosts: [] });
      const wheelBytes = readFileSync(WHEEL_PATH);
      await session.writeFile(
        "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
        wheelBytes,
      );
      const result = await session.installPythonPackages(
        "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
      );
      assert.ok(result.installed.length > 0);
    },
  );

  // ── HTTP(S) URL install via local server ─────────────────────

  it(
    "installs a wheel from HTTP URL via local server",
    { skip: SKIP, timeout: 120_000 },
    async () => {
      // Start a local HTTP server serving the wheel fixture
      const wheelBytes = readFileSync(WHEEL_PATH);
      const server = http.createServer((req, res) => {
        res.writeHead(200, { "Content-Type": "application/octet-stream" });
        res.end(wheelBytes);
      });
      await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
      const port = server.address().port;

      try {
        const session = await openSession({
          allowedHosts: [`127.0.0.1:${port}`],
        });
        const url = `http://127.0.0.1:${port}/wasmsh_test_fixture-0.1.0-py3-none-any.whl`;
        const result = await session.installPythonPackages(url);
        assert.ok(result.installed.length > 0);

        // Verify the package is importable
        const run = await session.run(
          "python3 -c \"from wasmsh_test_fixture import GREETING; print(GREETING)\"",
        );
        assert.equal(run.exitCode, 0, `python3 failed: ${run.stderr}`);
        assert.ok(run.stdout.includes("hello from wasmsh_test_fixture"));
      } finally {
        server.close();
      }
    },
  );

  // ── PyPI package name install ────────────────────────────────

  it(
    "installs a pure-Python package by name from PyPI",
    { skip: SKIP || process.env.SKIP_NETWORK === "1", timeout: 120_000 },
    async () => {
      const session = await openSession({
        allowedHosts: ["cdn.jsdelivr.net", "pypi.org", "files.pythonhosted.org"],
      });
      // six is a tiny pure-Python package with no dependencies
      const result = await session.installPythonPackages("six");
      assert.ok(result.installed.length > 0);

      const run = await session.run(
        "python3 -c \"import six; print(six.__version__)\"",
      );
      assert.equal(run.exitCode, 0, `python3 failed: ${run.stderr}`);
      assert.ok(run.stdout.trim().length > 0, "should print version");
    },
  );

  it(
    "rejects non-existent package",
    { skip: SKIP || process.env.SKIP_NETWORK === "1", timeout: 120_000 },
    async () => {
      const session = await openSession({
        allowedHosts: ["cdn.jsdelivr.net", "pypi.org", "files.pythonhosted.org"],
      });
      await assert.rejects(
        () =>
          session.installPythonPackages(
            "wasmsh-nonexistent-package-xyz-123456",
          ),
        /not found|can't find|ValueError/i,
        "should report package not found",
      );
    },
  );
});
