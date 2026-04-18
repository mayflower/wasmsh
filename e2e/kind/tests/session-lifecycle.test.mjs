import { test } from "node:test";
import assert from "node:assert/strict";

import { createDispatcherClient } from "../lib/dispatcher-client.mjs";

const DISPATCHER_URL = process.env.WASMSH_E2E_DISPATCHER_URL;
if (!DISPATCHER_URL) {
  throw new Error("WASMSH_E2E_DISPATCHER_URL must be set by the kind e2e orchestrator");
}

// Pyodide snapshot restore takes ~5-20s on a warm runner and can exceed 60s
// on a cold pod, so each call gets a generous budget.
const SESSION_REQUEST_TIMEOUT_MS = 120_000;
const dispatcher = createDispatcherClient({
  baseUrl: DISPATCHER_URL,
  defaultTimeoutMs: SESSION_REQUEST_TIMEOUT_MS,
});

function base64(text) {
  return Buffer.from(text, "utf8").toString("base64");
}

test("end-to-end session: create → run echo → write → read → close", async (t) => {
  const create = await dispatcher.createSession({
    session_id: `e2e-${Date.now()}`,
    allowed_hosts: [],
    step_budget: 0,
    initial_files: [],
  });
  assert.equal(create.status, 201, `create session failed: ${JSON.stringify(create.body)}`);
  const sessionId = create.body?.session?.sessionId ?? create.body?.sessionId;
  assert.ok(sessionId, `dispatcher did not return a session id: ${JSON.stringify(create.body)}`);

  t.after(async () => {
    // Best-effort cleanup — even if intermediate assertions fail the runner
    // should release its slot, so swallow only the expected "not found".
    const close = await dispatcher.closeSession(sessionId);
    if (close.status !== 200 && close.status !== 404) {
      console.error(`closeSession returned ${close.status}: ${JSON.stringify(close.body)}`);
    }
  });

  const run = await dispatcher.runInSession(sessionId, "echo hello-from-kind");
  assert.equal(run.status, 200, `run failed: ${JSON.stringify(run.body)}`);
  const stdout = run.body?.result?.stdout ?? run.body?.result?.output ?? "";
  assert.match(
    typeof stdout === "string" ? stdout : JSON.stringify(stdout),
    /hello-from-kind/,
    `expected echo output, got ${JSON.stringify(run.body)}`,
  );

  const payload = "kind-e2e-payload";
  const write = await dispatcher.writeFile(sessionId, "/workspace/kind.txt", base64(payload));
  assert.equal(write.status, 200, `writeFile failed: ${JSON.stringify(write.body)}`);

  const read = await dispatcher.readFile(sessionId, "/workspace/kind.txt");
  assert.equal(read.status, 200, `readFile failed: ${JSON.stringify(read.body)}`);
  const contentBase64 = read.body?.result?.contentBase64;
  assert.ok(contentBase64, `readFile missing contentBase64: ${JSON.stringify(read.body)}`);
  assert.equal(Buffer.from(contentBase64, "base64").toString("utf8"), payload);
});

test("creating a second session in parallel does not block the first", async () => {
  const idA = `e2e-a-${Date.now()}`;
  const idB = `e2e-b-${Date.now()}`;
  const [a, b] = await Promise.all([
    dispatcher.createSession({
      session_id: idA,
      allowed_hosts: [],
      step_budget: 0,
      initial_files: [],
    }),
    dispatcher.createSession({
      session_id: idB,
      allowed_hosts: [],
      step_budget: 0,
      initial_files: [],
    }),
  ]);
  try {
    assert.equal(a.status, 201, `session A failed: ${JSON.stringify(a.body)}`);
    assert.equal(b.status, 201, `session B failed: ${JSON.stringify(b.body)}`);
  } finally {
    await Promise.allSettled([
      dispatcher.closeSession(a.body?.session?.sessionId ?? idA),
      dispatcher.closeSession(b.body?.session?.sessionId ?? idB),
    ]);
  }
});
