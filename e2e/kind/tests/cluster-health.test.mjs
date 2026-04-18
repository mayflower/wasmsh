import { test } from "node:test";
import assert from "node:assert/strict";

import { createDispatcherClient } from "../lib/dispatcher-client.mjs";
import { createKubectl, waitUntil } from "../lib/kubectl.mjs";

const DISPATCHER_URL = requireEnv("WASMSH_E2E_DISPATCHER_URL");
const KUBECONFIG = requireEnv("WASMSH_E2E_KUBECONFIG");
const KUBE_CONTEXT = requireEnv("WASMSH_E2E_KUBE_CONTEXT");
const NAMESPACE = requireEnv("WASMSH_E2E_NAMESPACE");
const RELEASE = requireEnv("WASMSH_E2E_RELEASE");

function requireEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} must be set by the kind e2e orchestrator`);
  return value;
}

const dispatcher = createDispatcherClient({ baseUrl: DISPATCHER_URL });
const kubectl = createKubectl({
  kubeconfig: KUBECONFIG,
  context: KUBE_CONTEXT,
  namespace: NAMESPACE,
});

test("dispatcher /healthz returns 200", async () => {
  const response = await dispatcher.healthz();
  assert.equal(response.status, 200);
  assert.equal(response.body?.ok, true);
});

test("dispatcher /readyz becomes 200 once runner pods are Ready", async () => {
  await waitUntil(
    async () => {
      const response = await dispatcher.readyz();
      return response.status === 200;
    },
    { intervalMs: 1500, timeoutMs: 60_000, description: "dispatcher readyz 200" },
  );
  const final = await dispatcher.readyz();
  assert.equal(final.status, 200);
});

test("helm release exposes the expected dispatcher + runner deployments", async () => {
  const dispatcherDeployment = await kubectl.getDeployment(`${RELEASE}-dispatcher`);
  assert.equal(dispatcherDeployment.status?.availableReplicas, dispatcherDeployment.spec.replicas);

  const runnerDeployment = await kubectl.getDeployment(`${RELEASE}-runner`);
  assert.ok(
    runnerDeployment.status?.availableReplicas >= 1,
    `expected at least one available runner replica, got ${runnerDeployment.status?.availableReplicas}`,
  );
});

test("all runner pods report Ready condition", async () => {
  const pods = await kubectl.getPods("app.kubernetes.io/component=runner");
  assert.ok(pods.items.length > 0, "expected at least one runner pod");
  for (const pod of pods.items) {
    const ready = pod.status?.conditions?.find((c) => c.type === "Ready");
    assert.equal(
      ready?.status,
      "True",
      `runner pod ${pod.metadata.name} not Ready: ${JSON.stringify(pod.status?.conditions)}`,
    );
  }
});
