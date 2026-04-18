/**
 * Network allowlist security tests — Pyodide Node.
 *
 * Verifies that curl/wget can only reach hosts in the allowed_hosts list.
 * Uses mayflower.de as the test target.
 *
 * These tests use the host adapter's raw C ABI protocol, which passes
 * allowed_hosts through HostCommand::Init to the Rust runtime.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it, after } from "node:test";
import assert from "node:assert/strict";

import { extractStream, getExitCode } from "../../../packages/npm/wasmsh-pyodide/lib/protocol.mjs";

const SKIP = process.env.SKIP_PYODIDE === "1";
let outboundHttpsAvailable;

function findStdout(events) {
  return new TextDecoder().decode(extractStream(events, "Stdout"));
}

function findStderr(events) {
  return new TextDecoder().decode(extractStream(events, "Stderr"));
}

function findExitCode(events) {
  return getExitCode(events);
}

async function ensureExternalHttpsReachable(t, url) {
  try {
    const response = await fetch(url, {
      redirect: "follow",
      signal: AbortSignal.timeout(5_000),
    });
    if (response.status >= 500) {
      throw new Error(`unexpected status ${response.status}`);
    }
    return true;
  } catch (error) {
    t.skip(`external network is unavailable for ${url}: ${error.message}`);
    return false;
  }
}

describe("network allowlist security (Pyodide Node)", () => {
  const adapters = [];

  async function createAdapter(allowedHosts) {
    const { createHostAdapter } = await import("../pyodide-host-adapter.mjs");
    const adapter = await createHostAdapter({
      fullPython: true,
      stepBudget: 0,
    });
    // Send Init with allowed_hosts (the constructor already sent Init with
    // default params, so we send a second Init to configure the allowlist).
    await adapter.send({
      Init: { step_budget: 0, allowed_hosts: allowedHosts },
    });
    adapters.push(adapter);
    return adapter;
  }

  after(() => {
    for (const a of adapters) {
      try {
        a.destroy();
      } catch {
        // ignore cleanup errors
      }
    }
  });

  // ── Allowed host ──────────────────────────────────────────────

  it(
    "curl to allowed host (mayflower.de) succeeds",
    { skip: SKIP, timeout: 60_000 },
    async (t) => {
      if (!(await ensureExternalHttpsReachable(t, "https://mayflower.de"))) {
        return;
      }
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "curl -sL https://mayflower.de" },
      });
      const stdout = findStdout(events);
      const exitCode = findExitCode(events);

      assert.equal(exitCode, 0, "curl to allowed host should succeed");
      assert.ok(stdout.length > 0, "should return content");
      assert.match(stdout, /<(!|html|HTML)/, "should contain HTML");
    },
  );

  it(
    "wget to allowed host (mayflower.de) succeeds",
    { skip: SKIP, timeout: 60_000 },
    async (t) => {
      if (!(await ensureExternalHttpsReachable(t, "https://mayflower.de"))) {
        return;
      }
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "wget -qO - https://mayflower.de" },
      });
      const stdout = findStdout(events);
      const exitCode = findExitCode(events);

      assert.equal(exitCode, 0, "wget to allowed host should succeed");
      assert.ok(stdout.length > 0, "should return content");
    },
  );

  // ── Denied host ───────────────────────────────────────────────

  it(
    "curl to denied host (example.com) is blocked",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "curl https://example.com" },
      });
      const stderr = findStderr(events);
      const exitCode = findExitCode(events);

      assert.notEqual(exitCode, 0, "curl to denied host must fail");
      assert.ok(stderr.includes("denied"), `should mention 'denied': ${stderr}`);
    },
  );

  it(
    "wget to denied host (example.com) is blocked",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "wget -qO - https://example.com" },
      });
      const stderr = findStderr(events);
      const exitCode = findExitCode(events);

      assert.notEqual(exitCode, 0, "wget to denied host must fail");
      assert.ok(stderr.includes("denied"), `should mention 'denied': ${stderr}`);
    },
  );

  // ── Subdomain blocked with exact match ────────────────────────

  it(
    "curl to subdomain is blocked with exact host match",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "curl https://evil.mayflower.de" },
      });
      const exitCode = findExitCode(events);
      assert.notEqual(exitCode, 0, "subdomain must be blocked");
    },
  );

  // ── Similar hostnames blocked ─────────────────────────────────

  it(
    "curl to similar-looking hostnames is blocked",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const adapter = await createAdapter(["mayflower.de"]);
      for (const host of [
        "https://notmayflower.de",
        "https://mayflower.de.evil.com",
        "https://mayflower.com",
      ]) {
        const events = await adapter.send({
          Run: { input: `curl ${host}` },
        });
        const exitCode = findExitCode(events);
        assert.notEqual(exitCode, 0, `${host} must be blocked`);
      }
    },
  );

  // ── Wildcard pattern ──────────────────────────────────────────

  it(
    "wildcard *.mayflower.de allows base domain but blocks others",
    { skip: SKIP, timeout: 60_000 },
    async (t) => {
      if (!(await ensureExternalHttpsReachable(t, "https://mayflower.de"))) {
        return;
      }
      const adapter = await createAdapter(["*.mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "curl -sL https://mayflower.de" },
      });
      assert.equal(findExitCode(events), 0, "base domain should be allowed");

      const events2 = await adapter.send({
        Run: { input: "curl https://example.com" },
      });
      assert.notEqual(
        findExitCode(events2),
        0,
        "example.com must still be blocked",
      );
    },
  );

  // ── Empty allowlist ───────────────────────────────────────────

  it(
    "empty allowlist blocks all hosts",
    { skip: SKIP, timeout: 60_000 },
    async () => {
      const adapter = await createAdapter([]);
      const events = await adapter.send({
        Run: { input: "curl https://mayflower.de" },
      });
      const stderr = findStderr(events);
      const exitCode = findExitCode(events);

      assert.notEqual(exitCode, 0, "empty allowlist must block");
      assert.ok(
        stderr.includes("denied") || stderr.includes("allowlist") || stderr.includes("not available"),
        `should mention denial or unavailability: ${stderr}`,
      );
    },
  );

  // ── curl piped to wc ─────────────────────────────────────────

  it(
    "curl piped to wc works with allowed host",
    { skip: SKIP, timeout: 60_000 },
    async (t) => {
      if (!(await ensureExternalHttpsReachable(t, "https://mayflower.de"))) {
        return;
      }
      const adapter = await createAdapter(["mayflower.de"]);
      const events = await adapter.send({
        Run: { input: "curl -sL https://mayflower.de | wc -l" },
      });
      const stdout = findStdout(events);
      const exitCode = findExitCode(events);

      assert.equal(exitCode, 0);
      const lines = parseInt(stdout.trim(), 10);
      assert.ok(lines > 5, `expected >5 lines, got ${lines}`);
    },
  );
});
