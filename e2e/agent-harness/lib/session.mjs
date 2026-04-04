/**
 * Thin wrapper around WasmshSandbox from the mayflower/deepagentsjs fork.
 */
import { WasmshSandbox } from "@langchain/wasmsh";

export async function createSandbox(options = {}) {
  const sandbox = await WasmshSandbox.createNode({
    workingDirectory: "/workspace",
    ...options,
  });
  return sandbox;
}
