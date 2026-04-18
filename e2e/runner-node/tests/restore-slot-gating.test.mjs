import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

test("runner caps concurrent restores at restoreSlots", async () => {
  let inflightRestores = 0;
  let maxInflightRestores = 0;
  let nextWorkerId = 0;
  const seenQueueDepths = [];

  const runner = await createRunner({
    restoreSlots: 2,
    startupWarmRestores: 0,
    async restoreSessionWorker({ metrics, queueDepth }) {
      seenQueueDepths.push(queueDepth);
      const restore = metrics.startRestore(queueDepth);
      restore.beginStage("worker_spawn");
      inflightRestores += 1;
      maxInflightRestores = Math.max(maxInflightRestores, inflightRestores);
      await sleep(20);
      restore.endStage("worker_spawn");
      restore.beginStage("sandbox_restore");
      await sleep(5);
      restore.endStage("sandbox_restore");
      inflightRestores = Math.max(0, inflightRestores - 1);

      const workerId = `fake-worker-${nextWorkerId}`;
      nextWorkerId += 1;

      return {
        id: `fake-session-${workerId}`,
        workerId,
        restoreMetrics: restore.finish(),
        sendRequest(method) {
          if (method === "close") {
            return Promise.resolve({ closed: true });
          }
          if (method === "run") {
            return Promise.resolve({ exitCode: 0, stdout: "", stderr: "" });
          }
          return Promise.resolve({});
        },
        waitForExit() {
          return Promise.resolve(0);
        },
        terminate() {
          return Promise.resolve(0);
        },
      };
    },
  });

  try {
    const sessions = await Promise.all(
      Array.from({ length: 6 }, () => runner.createSession()),
    );

    assert.equal(maxInflightRestores, 2);
    assert.ok(seenQueueDepths.some((depth) => depth > 0));
    assert.equal(runner.metrics.snapshot().wasmsh_active_sessions, 6);

    await Promise.all(sessions.map((session) => session.close()));
  } finally {
    await runner.close();
  }
});
