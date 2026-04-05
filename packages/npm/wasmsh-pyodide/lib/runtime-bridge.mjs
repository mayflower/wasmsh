function parseHostCommandResult(module, resultPtr) {
  const resultStr = module.UTF8ToString(resultPtr);
  module.ccall("wasmsh_runtime_free_string", null, ["number"], [resultPtr]);
  return JSON.parse(resultStr);
}

export function createRuntimeBridge(module) {
  if (module._wasmshRuntimeBridge) {
    return module._wasmshRuntimeBridge;
  }

  let handle = module.ccall("wasmsh_runtime_new", "number", [], []);
  if (handle === 0) {
    throw new Error("wasmsh_runtime_new returned null");
  }

  const bridge = {
    get handle() {
      return handle;
    },
    sendHostCommand(command) {
      if (handle === null) {
        throw new Error("runtime not initialized");
      }
      const jsonPtr = module.stringToNewUTF8(JSON.stringify(command));
      try {
        const resultPtr = module.ccall(
          "wasmsh_runtime_handle_json",
          "number",
          ["number", "number"],
          [handle, jsonPtr],
        );
        return parseHostCommandResult(module, resultPtr);
      } finally {
        module._free(jsonPtr);
      }
    },
    close() {
      if (handle === null) {
        return;
      }
      module.ccall("wasmsh_runtime_free", null, ["number"], [handle]);
      handle = null;
      if (module._wasmshRuntimeBridge === bridge) {
        delete module._wasmshRuntimeBridge;
      }
    },
  };

  module._wasmshRuntimeBridge = bridge;
  return bridge;
}
