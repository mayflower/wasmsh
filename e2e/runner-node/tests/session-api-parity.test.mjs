import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("runner sessions expose init/run/writeFile/readFile/listDir/close", async () => {
  const runner = await createRunner();
  try {
    const session = await runner.createSession({
      initialFiles: [
        {
          path: "/workspace/seed.txt",
          content: "seeded",
        },
      ],
    });

    const init = await session.init();
    assert.equal(init.workerId, session.workerId);

    const seeded = await session.readFile("/workspace/seed.txt");
    assert.equal(new TextDecoder().decode(seeded.content), "seeded");

    await session.writeFile("/workspace/out.txt", "hello");
    const output = await session.run("cat /workspace/out.txt");
    assert.match(output.stdout, /hello/);

    const listing = await session.listDir("/workspace");
    assert.match(listing.output, /seed.txt/);
    assert.match(listing.output, /out.txt/);

    await session.close();
  } finally {
    await runner.close();
  }
});
