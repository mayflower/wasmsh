import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("runner exports restore metrics and stage timings", async () => {
  const runner = await createRunner();
  try {
    const session = await runner.createSession();
    const metrics = runner.metrics.snapshot();
    assert.ok(metrics.wasmsh_session_restore_duration_ms.samples.length >= 1);
    assert.ok(metrics.wasmsh_restore_stage_duration_ms.worker_spawn.samples.length >= 1);
    assert.ok(metrics.wasmsh_restore_stage_duration_ms.sandbox_restore.samples.length >= 1);
    assert.equal(metrics.wasmsh_active_sessions, 1);
    await session.close();
  } finally {
    await runner.close();
  }
});
