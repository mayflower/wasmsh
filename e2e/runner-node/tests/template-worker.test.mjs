import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("runner boots exactly one template worker that never holds user sessions", async () => {
  const runner = await createRunner();
  try {
    const template = runner.getTemplateInfo();
    assert.match(template.workerId, /^template-/);
    assert.deepEqual(runner.listSessions(), []);
  } finally {
    await runner.close();
  }
});
