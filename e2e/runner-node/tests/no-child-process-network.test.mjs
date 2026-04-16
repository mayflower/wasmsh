import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

test("runner hotpath files do not embed child-process HTTP calls", async () => {
  const files = [
    "tools/runner-node/src/fetch-broker.mjs",
    "tools/runner-node/src/session-worker.mjs",
    "tools/runner-node/src/runner-main.mjs",
  ];

  for (const file of files) {
    const source = await readFile(resolve(process.cwd(), file), "utf8");
    assert.ok(!source.includes("execFileSync"));
    assert.ok(!source.includes("spawn("));
  }
});
