import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("restore latency smoke exposes a measurable p95", async () => {
  const runner = await createRunner();
  try {
    for (let index = 0; index < 3; index += 1) {
      const session = await runner.createSession();
      await session.close();
    }
    const metrics = runner.metrics.snapshot();
    assert.ok(metrics.wasmsh_session_restore_duration_ms.p95 > 0);
  } finally {
    await runner.close();
  }
});
