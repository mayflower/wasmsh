/**
 * Thin wrapper around WasmshSandbox from the mayflower/deepagentsjs fork.
 */
import { WasmshSandbox } from "@langchain/wasmsh";

export async function createSandbox() {
  const sandbox = await WasmshSandbox.createNode({
    workingDirectory: "/workspace",
  });
  return sandbox;
}
