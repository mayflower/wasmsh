import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("each session gets a fresh worker id", async () => {
  const runner = await createRunner();
  try {
    const first = await runner.createSession();
    const second = await runner.createSession();
    assert.notEqual(first.workerId, second.workerId);
    await first.close();
    await second.close();
  } finally {
    await runner.close();
  }
});
