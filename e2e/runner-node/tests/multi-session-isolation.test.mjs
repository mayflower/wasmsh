import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("sessions do not share workspace state or worker ids", async () => {
  const runner = await createRunner();
  try {
    const first = await runner.createSession();
    const second = await runner.createSession();

    await first.writeFile("/workspace/only-first.txt", "alpha");
    const firstRead = await first.readFile("/workspace/only-first.txt");
    assert.equal(new TextDecoder().decode(firstRead.content), "alpha");

    const secondResult = await second.run("cat /workspace/only-first.txt");
    assert.notEqual(secondResult.exitCode, 0);
    assert.notEqual(first.workerId, second.workerId);

    await first.close();
    await second.close();
  } finally {
    await runner.close();
  }
});
