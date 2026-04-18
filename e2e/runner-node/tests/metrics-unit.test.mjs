import { test } from "node:test";
import assert from "node:assert/strict";

import {
  createRunnerMetrics,
  renderPrometheusMetrics,
  REQUIRED_RUNNER_METRICS,
} from "../../../tools/runner-node/src/metrics.mjs";

test("REQUIRED_RUNNER_METRICS includes the labelled broker-error counter", () => {
  assert.ok(REQUIRED_RUNNER_METRICS.includes("wasmsh_broker_fetch_errors_total"));
  assert.ok(REQUIRED_RUNNER_METRICS.includes("wasmsh_snapshot_restore_failures_total"));
});

test("percentile returns 0 when no samples exist", () => {
  const metrics = createRunnerMetrics();
  const snap = metrics.snapshot();
  assert.equal(snap.wasmsh_session_restore_duration_ms.p95, 0);
});

test("renderPrometheusMetrics emits all required families and broker-error rows", () => {
  const metrics = createRunnerMetrics();
  metrics.brokerFetchError("host_denied");
  metrics.brokerFetchError("timeout");
  metrics.brokerFetchError("timeout");
  metrics.hostDenied();
  metrics.sessionOpened();
  metrics.sessionClosed();

  const output = renderPrometheusMetrics(metrics.snapshot());
  for (const name of REQUIRED_RUNNER_METRICS) {
    assert.ok(output.includes(`# HELP ${name}`), `missing HELP for ${name}`);
    assert.ok(output.includes(`# TYPE ${name}`), `missing TYPE for ${name}`);
  }
  assert.match(output, /wasmsh_broker_fetch_errors_total\{reason="host_denied"\} 1/);
  assert.match(output, /wasmsh_broker_fetch_errors_total\{reason="timeout"\} 2/);
  assert.match(output, /wasmsh_allowed_host_denied_total 1/);
});

test("brokerFetchError uses 'unknown' as the label when called without a reason", () => {
  const metrics = createRunnerMetrics();
  metrics.brokerFetchError();
  metrics.brokerFetchError("");
  const snap = metrics.snapshot();
  assert.equal(snap.wasmsh_broker_fetch_errors_total.unknown, 2);
});

test("startRestore tracks stages and restoreFailures counter", () => {
  const metrics = createRunnerMetrics();
  const restoreA = metrics.startRestore(3);
  restoreA.beginStage("worker_spawn");
  restoreA.endStage("worker_spawn");
  restoreA.beginStage("sandbox_restore");
  restoreA.endStage("sandbox_restore");
  const finished = restoreA.finish();
  assert.ok(finished.total >= 0);
  assert.ok("worker_spawn" in finished.stages);
  assert.ok("sandbox_restore" in finished.stages);

  const restoreB = metrics.startRestore(2);
  restoreB.beginStage("worker_spawn");
  restoreB.fail();

  const snap = metrics.snapshot();
  assert.equal(snap.wasmsh_snapshot_restore_failures_total, 1);
  assert.ok(
    snap.wasmsh_session_restore_duration_ms.samples.length >= 1,
    "successful restore durations must be recorded",
  );
  assert.ok(
    snap.wasmsh_restore_stage_duration_ms.worker_spawn.samples.length >= 1,
    "stage durations must be recorded on success",
  );
});

test("sessionClosed cannot drive active sessions negative", () => {
  const metrics = createRunnerMetrics();
  metrics.sessionClosed();
  metrics.sessionClosed();
  const snap = metrics.snapshot();
  assert.equal(snap.wasmsh_active_sessions, 0);
});

test("inflightRestores reflects concurrent starts", () => {
  const metrics = createRunnerMetrics();
  metrics.startRestore(1);
  metrics.startRestore(2);
  const snap = metrics.snapshot();
  assert.equal(snap.wasmsh_inflight_restores, 2);
});

test("endStage without a matching beginStage is a no-op", () => {
  const metrics = createRunnerMetrics();
  const restore = metrics.startRestore(0);
  restore.endStage("nonexistent");
  const finished = restore.finish();
  assert.deepEqual(finished.stages, {});
});
