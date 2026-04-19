// End-to-end test exercising the `WasmshRemoteSandbox` TypeScript client
// against a live helm-installed dispatcher + runner inside kind.
//
// The orchestrator (`scripts/run.mjs`) brings the cluster up, installs the
// chart, opens a port-forward to the dispatcher service, and sets
// `WASMSH_E2E_DISPATCHER_URL` to that local URL.  All we need here is to
// point the sandbox client at it and verify the full contract the LangChain
// Deep Agents backend relies on: session creation, bash execute with exit
// code, file upload + download round-trip, grep-via-execute, and cleanup.
import { test } from "node:test";
import assert from "node:assert/strict";

import { WasmshRemoteSandbox } from "@mayflowergmbh/langchain-wasmsh";

const DISPATCHER_URL = process.env.WASMSH_E2E_DISPATCHER_URL;
if (!DISPATCHER_URL) {
  throw new Error(
    "WASMSH_E2E_DISPATCHER_URL must be set by the kind e2e orchestrator",
  );
}

test("WasmshRemoteSandbox executes bash through the kind dispatcher", async (t) => {
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
  });
  t.after(() => sandbox.stop());

  const result = await sandbox.execute("echo hello-from-kind && python3 -c 'print(2+2)'");
  assert.equal(result.exitCode, 0, `non-zero exit: ${JSON.stringify(result)}`);
  assert.match(result.output, /hello-from-kind/);
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
    ["/workspace/kind-roundtrip.bin", payload],
  ]);
  assert.equal(uploads[0].error, null, `upload failed: ${uploads[0].error}`);

  const downloads = await sandbox.downloadFiles(["/workspace/kind-roundtrip.bin"]);
  assert.equal(downloads[0].error, null, `download failed: ${downloads[0].error}`);
  assert.deepEqual(
    Array.from(downloads[0].content),
    Array.from(payload),
    "downloaded bytes differ from upload",
  );
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
  const seed = new TextEncoder().encode("kind seed\n");
  const sandbox = await WasmshRemoteSandbox.create({
    dispatcherUrl: DISPATCHER_URL,
    initialFiles: { "/workspace/seed.txt": seed },
  });
  t.after(() => sandbox.stop());

  const result = await sandbox.execute("cat seed.txt");
  assert.equal(result.exitCode, 0);
  assert.match(result.output, /kind seed/);
});
