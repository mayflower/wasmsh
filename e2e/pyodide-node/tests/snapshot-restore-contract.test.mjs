import { test } from "node:test";
import assert from "node:assert/strict";
import { resolve } from "node:path";

import { buildSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/builder.mjs";
import { restoreFromSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/restore.mjs";

const assetDir = resolve(process.cwd(), "packages/npm/wasmsh-pyodide/assets");

test("restoreFromSnapshot preserves the runtime contract", async () => {
  const artifact = await buildSnapshot({ assetDir });
  const session = await restoreFromSnapshot({
    assetDir,
    snapshotBytes: artifact.memoryBytes,
  });

  try {
    const echo = await session.run("echo hello");
    assert.equal(echo.exitCode, 0);
    assert.match(echo.stdout, /hello/);

    const python = await session.run("python3 -c \"print(1+1)\"");
    assert.equal(python.exitCode, 0);
    assert.match(python.stdout, /2/);

    await session.writeFile("/workspace/greeting.txt", new TextEncoder().encode("hello snapshot"));
    const readBack = await session.readFile("/workspace/greeting.txt");
    assert.equal(new TextDecoder().decode(readBack.content), "hello snapshot");
  } finally {
    session.close();
  }
});
