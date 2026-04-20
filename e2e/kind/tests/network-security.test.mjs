// Network allowlist security tests against the kind-installed
// dispatcher + runner stack.
//
// This mirrors e2e/dispatcher-compose/tests/network-security.test.mjs
// but runs against the production-parity Helm install rather than
// docker-compose.  The default production chart ships a NetworkPolicy
// that only allows DNS egress from runner pods; e2e/values-e2e.yaml
// disables it (`networkPolicy.enabled: false`) so this suite isolates
// the sandbox-level allowlist enforcement rather than testing the
// policy+sandbox stack together.  A separate NetworkPolicy contract
// test would belong in k8s-manifest-contract.test.mjs.
//
// Skip-on-no-outbound is preserved because kind nodes in CI may run in
// network-restricted environments even without the NetworkPolicy.
import { after, test } from "node:test";
import assert from "node:assert/strict";

import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const DISPATCHER_URL = process.env.WASMSH_E2E_DISPATCHER_URL;
if (!DISPATCHER_URL) {
  throw new Error(
    "WASMSH_E2E_DISPATCHER_URL must be set by the kind e2e orchestrator",
  );
}

const ALLOWED_HOST = "mayflower.de";
const DENIED_HOST = "example.com";

let outboundReachable = null;

async function ensureOutbound(t) {
  if (outboundReachable !== null) {
    if (!outboundReachable) t.skip(`outbound HTTPS to ${ALLOWED_HOST} unreachable`);
    return outboundReachable;
  }
  // Probe via the sandbox itself: if `curl` with the allowlist permitting
  // the target still can't reach it, the cluster/node has no egress and
  // the allow-path assertions are meaningless.
  const probe = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
    allowedHosts: [ALLOWED_HOST],
  });
  try {
    const result = await probe.execute(
      `curl -sL --max-time 10 -o /dev/null -w '%{http_code}' https://${ALLOWED_HOST}`,
    );
    const code = Number.parseInt(result.output.trim(), 10);
    outboundReachable = result.exitCode === 0 && Number.isFinite(code) && code < 500;
  } catch {
    outboundReachable = false;
  } finally {
    await probe.stop().catch(() => {});
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
  assert.match(result.output, /<(!|html|HTML)/, "body should contain HTML");
});

test("denied host: curl is blocked by the runner broker", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl https://${DENIED_HOST} 2>&1`);
  assert.notEqual(result.exitCode, 0, "denied host must not succeed");
  assert.match(result.output, /denied|allowlist|not available/i,
    `denial diagnostic expected, got: ${JSON.stringify(result)}`);
});

test("subdomain: exact host match does not imply subdomain access", async () => {
  const sb = await sandbox([ALLOWED_HOST]);
  const result = await sb.execute(`curl https://evil.${ALLOWED_HOST} 2>&1`);
  assert.notEqual(result.exitCode, 0, "subdomain must be blocked");
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
