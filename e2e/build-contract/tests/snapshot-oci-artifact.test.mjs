import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

const repoRoot = process.cwd();
const workflowPath = resolve(repoRoot, ".github/workflows/snapshot-runner.yml");
const runnerDeploymentPath = resolve(repoRoot, "deploy/k8s/runner-deployment.yaml");

test("snapshot workflow and runtime config use immutable artifact references", () => {
  const workflow = readFileSync(workflowPath, "utf8");
  const deployment = readFileSync(runnerDeploymentPath, "utf8");

  assert.match(workflow, /just build-snapshot/);
  assert.match(workflow, /snapshot\.manifest\.json/);
  assert.match(workflow, /memory\.bin\.zst/);
  assert.match(workflow, /@sha256:/);
  assert.ok(!workflow.includes(":latest"));

  assert.match(deployment, /WASMSH_SNAPSHOT_REF/);
  assert.match(deployment, /@sha256:/);
  assert.ok(!deployment.includes(":latest"));
});
