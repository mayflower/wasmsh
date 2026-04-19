/**
 * Integration tests for WasmshRemoteSandbox against a live dispatcher.
 *
 * Requires `WASMSH_DISPATCHER_URL` to point at a running wasmsh dispatcher
 * with at least one bound runner.  `deploy/docker/compose.dispatcher-test.yml`
 * spins up a suitable stack on localhost:8080.
 */
import { sandboxStandardTests } from "@langchain/sandbox-standard-tests/vitest";

import { WasmshRemoteSandbox } from "./remote.js";

const dispatcherUrl = process.env.WASMSH_DISPATCHER_URL;

sandboxStandardTests({
  name: "WasmshRemoteSandbox",
  skip: !dispatcherUrl,
  timeout: 60_000,
  sequential: true,
  createSandbox: async (options) =>
    WasmshRemoteSandbox.create({
      dispatcherUrl: dispatcherUrl!,
      initialFiles: options?.initialFiles,
      workingDirectory: "/workspace",
    }),
  closeSandbox: (sandbox) => sandbox.stop(),
  resolvePath: (name) => `/workspace/${name}`,
});
