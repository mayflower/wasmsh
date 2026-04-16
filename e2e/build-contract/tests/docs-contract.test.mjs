import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

const repoRoot = process.cwd();
const runnerLocal = readFileSync(resolve(repoRoot, "docs/how-to/runner-local.md"), "utf8");
const metricsRef = readFileSync(resolve(repoRoot, "docs/reference/runner-metrics.md"), "utf8");
const architecture = readFileSync(resolve(repoRoot, "docs/explanation/snapshot-runner.md"), "utf8");
const runbook = readFileSync(resolve(repoRoot, "docs/how-to/runner-runbook.md"), "utf8");

test("runner docs include how-to, architecture, metrics, and runbook guidance", () => {
  assert.match(runnerLocal, /just build-snapshot/);
  assert.match(runnerLocal, /just test-e2e-runner-node/);

  assert.match(metricsRef, /wasmsh_session_restore_duration_ms/);
  assert.match(metricsRef, /wasmsh_allowed_host_denied_total/);

  assert.match(architecture, /fresh worker per session/i);
  assert.match(architecture, /single template/i);
  assert.match(architecture, /allowed_hosts/);

  assert.match(runbook, /Failure Modes/);
  assert.match(runbook, /readyz/);
});
