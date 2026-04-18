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
const workerId = `session-${threadId}`;

let session;
try {
  session = await restoreFromSnapshot({
    assetDir: workerData.assetDir,
    snapshotBytes,
    allowedHosts: workerData.allowedHosts,
    stepBudget: workerData.stepBudget,
    initialFiles: workerData.initialFiles,
    fetchHandlerSync: brokerClient ? brokerClient.fetchSync.bind(brokerClient) : deniedFetch,
    compiledWasmModule: workerData.compiledWasmModule,
  });
} catch (error) {
  // Surface the restore failure with full stage context before the
  // worker exits; otherwise the parent only sees a generic "exited
  // with code N" and cannot tell OOM from a corrupt snapshot.
  parentPort.postMessage({
    type: "error",
    stage: "restore",
    error: error instanceof Error ? error.message : String(error),
  });
  throw error;
}

parentPort.postMessage({
  type: "ready",
  workerId,
});

function closeSession() {
  if (!session) {
    return;
  }
  try {
    session.close();
  } finally {
    session = null;
    parentPort.removeAllListeners("message");
  }
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
  const isClose = message.method === "close";
  try {
    const result = await handleRequest(message);
    parentPort.postMessage({
      type: "response",
      id: message.id,
      ok: true,
      result,
    });
  } catch (error) {
    parentPort.postMessage({
      type: "response",
      id: message.id,
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    });
  } finally {
    // Always tear the worker down after any close attempt, successful
    // or not.  Previously only the success branch scheduled exit, so a
    // close whose handler threw left a zombie worker running forever
    // — subsequent requests to the session would never see the session
    // close complete.  process.exit inside a Worker only terminates
    // the worker thread (not the parent runner) per Node docs.
    if (isClose) {
      setImmediate(() => process.exit(0));
    }
  }
});
