import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("closed sessions are terminated instead of being pooled", async () => {
  const runner = await createRunner();
  try {
    const first = await runner.createSession();
    const firstWorkerId = first.workerId;
    await first.close();
    assert.equal(runner.metrics.snapshot().wasmsh_active_sessions, 0);

    const second = await runner.createSession();
    assert.notEqual(second.workerId, firstWorkerId);
    await second.close();
  } finally {
    await runner.close();
  }
});
