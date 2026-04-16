import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

const repoRoot = process.cwd();
const runnerDeployment = readFileSync(resolve(repoRoot, "deploy/k8s/runner-deployment.yaml"), "utf8");
const dispatcherDeployment = readFileSync(resolve(repoRoot, "deploy/k8s/dispatcher-deployment.yaml"), "utf8");
const hpa = readFileSync(resolve(repoRoot, "deploy/k8s/hpa.yaml"), "utf8");
const networkPolicy = readFileSync(resolve(repoRoot, "deploy/k8s/networkpolicy.yaml"), "utf8");

test("k8s artifacts model readiness, restore capacity, and egress policy without warm pools", () => {
  assert.match(runnerDeployment, /kind:\s*Deployment/);
  assert.match(runnerDeployment, /readinessProbe:/);
  assert.match(runnerDeployment, /path:\s*\/readyz/);
  assert.match(runnerDeployment, /WASMSH_RESTORE_SLOTS/);
  assert.match(runnerDeployment, /WASMSH_SNAPSHOT_REF/);
  assert.ok(!/warm[_-]?pool/i.test(runnerDeployment));

  assert.match(dispatcherDeployment, /kind:\s*Deployment/);
  assert.match(dispatcherDeployment, /wasmsh-dispatcher/);
  assert.ok(!/runtime[_-]?type/i.test(dispatcherDeployment));

  assert.match(hpa, /kind:\s*HorizontalPodAutoscaler/);
  assert.match(hpa, /wasmsh_inflight_restores/);

  assert.match(networkPolicy, /kind:\s*NetworkPolicy/);
  assert.match(networkPolicy, /Egress/);
});
