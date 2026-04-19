// End-to-end test exercising the `WasmshRemoteSandbox` TypeScript client
// against a docker-compose dispatcher+runner stack.  The orchestrator in
// `scripts/run.mjs` starts the stack, sets `WASMSH_E2E_DISPATCHER_URL`,
// and tears everything down afterwards.
//
// This suite mirrors the kind variant (`e2e/kind/tests/langchain-wasmsh-
// sandbox.test.mjs`) so both deployment targets stay in parity.
import { test } from "node:test";
import assert from "node:assert/strict";

import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const DISPATCHER_URL = process.env.WASMSH_E2E_DISPATCHER_URL;
if (!DISPATCHER_URL) {
  throw new Error(
    "WASMSH_E2E_DISPATCHER_URL must be set by the dispatcher-compose orchestrator",
  );
}

test("WasmshRemoteSandbox executes bash through the compose dispatcher", async (t) => {
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
  });
  t.after(() => sandbox.stop());

  const result = await sandbox.execute("echo hello-from-compose && python3 -c 'print(2+2)'");
  assert.equal(result.exitCode, 0, `non-zero exit: ${JSON.stringify(result)}`);
  assert.match(result.output, /hello-from-compose/);
  assert.match(result.output, /\b4\b/);
});

test("WasmshRemoteSandbox round-trips binary files", async (t) => {
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
  });
  t.after(() => sandbox.stop());

  const payload = new Uint8Array(1024);
  for (let i = 0; i < payload.length; i++) payload[i] = i % 256;

  const uploads = await sandbox.uploadFiles([
    ["/workspace/compose-roundtrip.bin", payload],
  ]);
  assert.equal(uploads[0].error, null);

  const downloads = await sandbox.downloadFiles(["/workspace/compose-roundtrip.bin"]);
  assert.equal(downloads[0].error, null);
  assert.deepEqual(Array.from(downloads[0].content), Array.from(payload));
});

test("WasmshRemoteSandbox reports a non-zero exit code", async (t) => {
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
  });
  t.after(() => sandbox.stop());

  const result = await sandbox.execute("exit 42");
  assert.equal(result.exitCode, 42);
});

test("WasmshRemoteSandbox seeds initial files at session creation", async (t) => {
  const seed = new TextEncoder().encode("compose seed\n");
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
    initialFiles: { "/workspace/seed.txt": seed },
  });
  t.after(() => sandbox.stop());

  const result = await sandbox.execute("cat seed.txt");
  assert.equal(result.exitCode, 0);
  assert.match(result.output, /compose seed/);
});
