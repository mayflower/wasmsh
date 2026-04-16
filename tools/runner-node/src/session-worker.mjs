import { parentPort, threadId, workerData } from "node:worker_threads";

import { createBrokerClient } from "./fetch-broker.mjs";
import { restoreFromSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/restore.mjs";

const brokerClient = createBrokerClient({
  parentPort,
  controlBuffer: workerData.controlBuffer,
  requestBuffer: workerData.requestBuffer,
  responseBuffer: workerData.responseBuffer,
});

const session = await restoreFromSnapshot({
  assetDir: workerData.assetDir,
  snapshotBytes: workerData.snapshotBytes,
  allowedHosts: workerData.allowedHosts,
  stepBudget: workerData.stepBudget,
  initialFiles: workerData.initialFiles,
  fetchHandlerSync: brokerClient.fetchSync.bind(brokerClient),
});

const workerId = `session-${threadId}`;
parentPort.postMessage({
  type: "ready",
  workerId,
});

async function handleRequest(message) {
  switch (message.method) {
    case "init":
      return { workerId };
    case "run":
      return session.run(message.params.command);
    case "writeFile":
      return session.writeFile(message.params.path, message.params.content);
    case "readFile":
      return session.readFile(message.params.path);
    case "listDir":
      return session.listDir(message.params.path);
    case "close":
      session.close();
      return { closed: true };
    default:
      throw new Error(`unknown method: ${message.method}`);
  }
}

parentPort.on("message", async (message) => {
  if (message?.type !== "request") {
    return;
  }
  try {
    const result = await handleRequest(message);
    parentPort.postMessage({
      type: "response",
      id: message.id,
      ok: true,
      result,
    });
    if (message.method === "close") {
      setImmediate(() => process.exit(0));
    }
  } catch (error) {
    parentPort.postMessage({
      type: "response",
      id: message.id,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    });
  }
});
