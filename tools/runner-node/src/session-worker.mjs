import { parentPort, threadId, workerData } from "node:worker_threads";

import { createBrokerClient } from "./fetch-broker.mjs";
import { restoreFromSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/restore.mjs";

function deniedFetch() {
  return {
    status: 0,
    headers: [],
    body_base64: "",
    error: "network access is disabled for this session",
  };
}

const brokerClient = workerData.controlBuffer
  ? createBrokerClient({
    parentPort,
    controlBuffer: workerData.controlBuffer,
    requestBuffer: workerData.requestBuffer,
    responseBuffer: workerData.responseBuffer,
  })
  : null;

const snapshotBytes = new Uint8Array(workerData.snapshotBuffer, 0, workerData.snapshotLength);

let session = await restoreFromSnapshot({
  assetDir: workerData.assetDir,
  snapshotBytes,
  allowedHosts: workerData.allowedHosts,
  stepBudget: workerData.stepBudget,
  initialFiles: workerData.initialFiles,
  fetchHandlerSync: brokerClient ? brokerClient.fetchSync.bind(brokerClient) : deniedFetch,
  compiledWasmModule: workerData.compiledWasmModule,
});

const workerId = `session-${threadId}`;
parentPort.postMessage({
  type: "ready",
  workerId,
});

function closeSession() {
  if (!session) {
    return;
  }
  session.close();
  session = null;
  parentPort.removeAllListeners("message");
}

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
      closeSession();
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
