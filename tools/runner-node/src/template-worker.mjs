import { parentPort, threadId, workerData } from "node:worker_threads";

import { buildSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/builder.mjs";

const artifact = await buildSnapshot({
  assetDir: workerData.assetDir,
  compiledWasmModule: workerData.compiledWasmModule,
});

parentPort.postMessage({
  type: "ready",
  workerId: `template-${threadId}`,
  manifest: artifact.manifest,
  selftest: artifact.selftest,
  snapshotBytes: artifact.memoryBytes,
});

parentPort.on("message", (message) => {
  if (message?.type === "close") {
    process.exit(0);
  }
  parentPort.postMessage({
    type: "error",
    error: "template worker does not accept user traffic",
  });
});
