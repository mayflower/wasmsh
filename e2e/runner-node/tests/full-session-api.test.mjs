import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("the production runner path handles the full session api and close errors cleanly", async () => {
  const runner = await createRunner();
  try {
    const session = await runner.createSession({
      initialFiles: [
        {
          path: "/workspace/input.txt",
          content: "runner",
        },
      ],
    });

    const read = await session.readFile("/workspace/input.txt");
    assert.equal(new TextDecoder().decode(read.content), "runner");

    const result = await session.run("python3 -c \"print('ok')\"");
    assert.equal(result.exitCode, 0);
    assert.match(result.stdout, /ok/);

    await session.close();
    await assert.rejects(() => session.run("echo after-close"));
  } finally {
    await runner.close();
  }
});
