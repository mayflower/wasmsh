// Network allowlist security tests for the scalable (dispatcher +
// runner) deployment path, exercised through `WasmshRemoteSandbox`.
//
// The raw allowlist enforcement is unit-tested in
// e2e/runner-node/tests/broker-policy.test.mjs and functionally tested
// against the in-process Pyodide paths in
// e2e/pyodide-node/tests/network-security.test.mjs and the two
// browser suites.  This file closes the gap on the HTTP path: the
// dispatcher must forward `allowed_hosts` from the create-session
// request into the runner's session-init, and the runner's fetch
// broker must then enforce the list for curl/wget the same way the
// in-process runtime does.
//
// Mirrors the phase shape of e2e/pyodide-node/tests/network-security.test.mjs.
import { after, before, test } from "node:test";
import assert from "node:assert/strict";

import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const DISPATCHER_URL = process.env.WASMSH_E2E_DISPATCHER_URL;
if (!DISPATCHER_URL) {
  throw new Error(
    "WASMSH_E2E_DISPATCHER_URL must be set by the dispatcher-compose orchestrator",
  );
}

const ALLOWED_HOST = "mayflower.de";
const DENIED_HOST = "example.com";

// The sandbox lets curl fetch a host only when the runner's fetch broker
// allows it.  If the CI network can't reach the reference host at all, the
// allow-case assertions would fail for network reasons rather than
// allowlist reasons — skip cleanly when that happens.
let outboundReachable = null;

async function ensureOutbound(t) {
  if (outboundReachable !== null) {
    if (!outboundReachable) t.skip(`outbound HTTPS to ${ALLOWED_HOST} unreachable`);
    return outboundReachable;
  }
  try {
    const resp = await fetch(`https://${ALLOWED_HOST}`, {
      redirect: "follow",
      signal: AbortSignal.timeout(5_000),
    });
    outboundReachable = resp.status < 500;
  } catch {
    outboundReachable = false;
  }
  if (!outboundReachable) t.skip(`outbound HTTPS to ${ALLOWED_HOST} unreachable`);
  return outboundReachable;
}

const sandboxes = [];
async function sandbox(allowedHosts) {
  const sb = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
    allowedHosts,
  });
  sandboxes.push(sb);
  return sb;
}

after(async () => {
  await Promise.all(sandboxes.map((s) => s.stop().catch(() => {})));
});

test("allowed host: curl through the dispatcher succeeds", async (t) => {
  if (!(await ensureOutbound(t))) return;
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl -sL https://${ALLOWED_HOST}`);
  assert.equal(result.exitCode, 0, `expected 0, got: ${JSON.stringify(result)}`);
  assert.ok(result.output.length > 0, "should return some body");
  assert.match(result.output, /<(!|html|HTML)/, "body should contain HTML");
});

test("allowed host: wget through the dispatcher succeeds", async (t) => {
  if (!(await ensureOutbound(t))) return;
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`wget -qO - https://${ALLOWED_HOST}`);
  assert.equal(result.exitCode, 0, `expected 0, got: ${JSON.stringify(result)}`);
  assert.ok(result.output.length > 0);
});

test("denied host: curl is blocked by the runner broker", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl https://${DENIED_HOST} 2>&1`);
  assert.notEqual(result.exitCode, 0, "denied host must not succeed");
  assert.match(result.output, /denied|allowlist|not available/i,
    `denial diagnostic expected, got: ${JSON.stringify(result)}`);
});

test("denied host: wget is blocked", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`wget -qO - https://${DENIED_HOST} 2>&1 || true`);
  // wget -q suppresses its own stderr; the shell sees a non-zero exit first.
  const probe = await sb.execute(`wget -O - https://${DENIED_HOST} 2>&1`);
  assert.notEqual(probe.exitCode, 0, "wget to denied host must fail");
  assert.match(probe.output, /denied|allowlist|not available/i);
});

test("subdomain: exact host match does not imply subdomain access", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl https://evil.${ALLOWED_HOST} 2>&1`);
  assert.notEqual(result.exitCode, 0, "subdomain must be blocked");
});

test("similar hostnames: lookalikes do not bypass the allowlist", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const cases = [
    `https://not${ALLOWED_HOST}`,
    `https://${ALLOWED_HOST}.evil.example`,
    "https://mayflower.com",
  ];
  for (const url of cases) {
    const result = await sb.execute(`curl ${url} 2>&1`);
    assert.notEqual(result.exitCode, 0, `${url} must be blocked`);
  }
});

test("wildcard: *.host allows base + subdomain, still blocks strangers", async (t) => {
  if (!(await ensureOutbound(t))) return;
  const sb = await sandbox([`*.${ALLOWED_HOST}`]);
  const allowed = await sb.execute(`curl -sL https://${ALLOWED_HOST}`);
  assert.equal(allowed.exitCode, 0, `wildcard should allow base domain: ${JSON.stringify(allowed)}`);

  const denied = await sb.execute(`curl https://${DENIED_HOST} 2>&1`);
  assert.notEqual(denied.exitCode, 0, "example.com must still be blocked under wildcard");
});

test("empty allowlist: every outbound fetch is denied", async () => {
  const sb = await sandbox([]);
  const result = await sb.execute(`curl https://${ALLOWED_HOST} 2>&1`);
  assert.notEqual(result.exitCode, 0, "empty allowlist must block everything");
  assert.match(result.output, /denied|allowlist|not available/i);
});

test("pipeline: allowed curl piped to wc still succeeds", async (t) => {
  if (!(await ensureOutbound(t))) return;
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl -sL https://${ALLOWED_HOST} | wc -l`);
  assert.equal(result.exitCode, 0);
  const lines = Number.parseInt(result.output.trim(), 10);
  assert.ok(lines > 5, `expected >5 lines, got ${lines}`);
});
