import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("small initial files are accepted and oversized inline payloads are rejected", async () => {
  const runner = await createRunner();
  try {
    const session = await runner.createSession({
      initialFiles: [
        {
          path: "/workspace/small.txt",
          content: "small",
        },
      ],
    });
    const file = await session.readFile("/workspace/small.txt");
    assert.equal(new TextDecoder().decode(file.content), "small");
    await session.close();

    await assert.rejects(
      () => runner.createSession({
        initialFiles: [
          {
            path: "/workspace/large.bin",
            content: "x".repeat(128 * 1024),
          },
        ],
      }),
      /too large/,
    );
  } finally {
    await runner.close();
  }
});
