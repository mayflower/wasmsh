import { test } from "node:test";
import assert from "node:assert/strict";

import { createDispatcherClient } from "../lib/dispatcher-client.mjs";
import { createKubectl, waitUntil } from "../lib/kubectl.mjs";

function requireEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} must be set by the kind e2e orchestrator`);
  return value;
}

const dispatcher = createDispatcherClient({
  baseUrl: requireEnv("WASMSH_E2E_DISPATCHER_URL"),
  defaultTimeoutMs: 120_000,
});
const kubectl = createKubectl({
  kubeconfig: requireEnv("WASMSH_E2E_KUBECONFIG"),
  context: requireEnv("WASMSH_E2E_KUBE_CONTEXT"),
  namespace: requireEnv("WASMSH_E2E_NAMESPACE"),
});
const RELEASE = requireEnv("WASMSH_E2E_RELEASE");
const RUNNER_DEPLOY = `${RELEASE}-runner`;

async function availableRunnerReplicas() {
  const deployment = await kubectl.getDeployment(RUNNER_DEPLOY);
  return deployment.status?.availableReplicas ?? 0;
}

test("scaling the runner deployment up and back down keeps the dispatcher ready", async (t) => {
  const initial = await kubectl.getDeployment(RUNNER_DEPLOY);
  const originalReplicas = initial.spec?.replicas ?? 2;
  t.after(async () => {
    // Always restore the original replica count so downstream tests run
    // against the deployment they expect.
    await kubectl.scaleDeployment(RUNNER_DEPLOY, originalReplicas);
    await waitUntil(
      async () => (await availableRunnerReplicas()) >= originalReplicas,
      { intervalMs: 2000, timeoutMs: 5 * 60 * 1000, description: `runner replicas back to ${originalReplicas}` },
    );
  });

  const target = originalReplicas + 1;
  await kubectl.scaleDeployment(RUNNER_DEPLOY, target);
  await waitUntil(
    async () => (await availableRunnerReplicas()) >= target,
    { intervalMs: 2000, timeoutMs: 5 * 60 * 1000, description: `runner replicas reach ${target}` },
  );

  const readyzDuringScale = await dispatcher.readyz();
  assert.equal(readyzDuringScale.status, 200);

  await kubectl.scaleDeployment(RUNNER_DEPLOY, originalReplicas);
  await waitUntil(
    async () => {
      const deployment = await kubectl.getDeployment(RUNNER_DEPLOY);
      return (
        (deployment.status?.availableReplicas ?? 0) === originalReplicas &&
        (deployment.status?.replicas ?? 0) === originalReplicas
      );
    },
    { intervalMs: 2000, timeoutMs: 5 * 60 * 1000, description: `runner replicas settle at ${originalReplicas}` },
  );

  const readyzAfter = await dispatcher.readyz();
  assert.equal(readyzAfter.status, 200);
});

test("deleting a runner pod does not take the dispatcher out of rotation", async () => {
  const before = await kubectl.getPods("app.kubernetes.io/component=runner");
  assert.ok(before.items.length >= 2, "resilience test needs ≥2 runner pods");
  const victim = before.items[0].metadata.name;

  await kubectl.deletePod(victim, { grace: 5 });

  // A session created immediately after the delete must still succeed — the
  // dispatcher should see the remaining runner as ready.
  const create = await dispatcher.createSession({
    session_id: `resilience-${Date.now()}`,
    allowed_hosts: [],
    step_budget: 0,
    initial_files: [],
  });
  try {
    assert.equal(create.status, 201, `create during pod delete failed: ${JSON.stringify(create.body)}`);
  } finally {
    const sessionId = create.body?.session?.sessionId;
    if (sessionId) {
      await dispatcher.closeSession(sessionId).catch(() => {});
    }
  }

  // And the deployment controller should bring the victim's replacement back.
  await waitUntil(
    async () => {
      const deployment = await kubectl.getDeployment(RUNNER_DEPLOY);
      return (
        (deployment.status?.availableReplicas ?? 0) === (deployment.spec?.replicas ?? 0)
      );
    },
    { intervalMs: 2000, timeoutMs: 5 * 60 * 1000, description: "runner deployment recovers after pod delete" },
  );
});
