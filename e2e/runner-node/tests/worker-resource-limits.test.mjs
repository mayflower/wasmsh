import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunner } from "../../../tools/runner-node/src/runner-main.mjs";

test("runner applies the tuned default worker resource limits", async () => {
  let capturedWorkerResourceLimits = null;

  const runner = await createRunner({
    startupWarmRestores: 0,
    async restoreSessionWorker({ metrics, workerResourceLimits }) {
      capturedWorkerResourceLimits = workerResourceLimits;
      const restore = metrics.startRestore(0);
      restore.beginStage("worker_spawn");
      restore.endStage("worker_spawn");
      restore.beginStage("sandbox_restore");
      restore.endStage("sandbox_restore");
      return {
        id: "fake-session",
        workerId: "fake-worker",
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
    const session = await runner.createSession();
    assert.deepEqual(capturedWorkerResourceLimits, {
      maxOldGenerationSizeMb: 48,
      maxYoungGenerationSizeMb: 8,
      stackSizeMb: 1,
      codeRangeSizeMb: 8,
    });
    await session.close();
  } finally {
    await runner.close();
  }
});
