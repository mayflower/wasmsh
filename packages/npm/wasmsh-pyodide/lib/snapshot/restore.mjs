import { createRuntimeBridge } from "../runtime-bridge.mjs";
import { createRestoredModuleFromSnapshot } from "../node-module.mjs";
import { buildRunResult, encodeBase64, extractStream } from "../protocol.mjs";
import { handlePipCommand, installPackages } from "../install.mjs";

function loadBundledPackageNames(assetDir, moduleRef) {
  const raw = moduleRef.FS.readFile(`${assetDir}/pyodide-lock.json`, { encoding: "utf8" });
  const lock = JSON.parse(raw);
  return new Set(
    Object.values(lock.packages || {})
      .map((entry) => entry.file_name)
      .filter(Boolean),
  );
}

export async function restoreFromSnapshot({
  assetDir,
  snapshotBytes,
  allowedHosts = [],
  stepBudget = 0,
  initialFiles = [],
  fetchHandlerSync,
  compiledWasmModule = null,
  wasmBytes = null,
}) {
  let module = await createRestoredModuleFromSnapshot(assetDir, snapshotBytes, {
    fetchHandlerSync,
    compiledWasmModule,
    wasmBytes,
  });
  let runtimeBridge = createRuntimeBridge(module);
  runtimeBridge.sendHostCommand({
    Init: { step_budget: stepBudget, allowed_hosts: allowedHosts },
  });

  for (const file of initialFiles) {
    runtimeBridge.sendHostCommand({
      WriteFile: {
        path: file.path,
        data: Array.from(file.content),
      },
    });
  }

  const bundled = new Set();
  let pyodide = module._pyodide;

  function requireOpen() {
    if (!module || !runtimeBridge || !pyodide) {
      throw new Error("session is closed");
    }
    return {
      module,
      runtimeBridge,
      pyodide,
    };
  }

  return {
    module,
    runtimeBridge,
    async run(command) {
      const { pyodide: activePyodide, runtimeBridge: activeRuntimeBridge } = requireOpen();
      const pipResult = await handlePipCommand(command, activePyodide, (opts) => this.installPythonPackages(opts));
      if (pipResult) {
        return pipResult;
      }
      return buildRunResult(activeRuntimeBridge.sendHostCommand({ Run: { input: command } }));
    },
    async writeFile(path, content) {
      const { runtimeBridge: activeRuntimeBridge } = requireOpen();
      const events = activeRuntimeBridge.sendHostCommand({
        WriteFile: {
          path,
          data: Array.from(content),
        },
      });
      return { events };
    },
    async readFile(path) {
      const { runtimeBridge: activeRuntimeBridge } = requireOpen();
      const events = activeRuntimeBridge.sendHostCommand({ ReadFile: { path } });
      return {
        events,
        content: extractStream(events, "Stdout"),
        contentBase64: encodeBase64(extractStream(events, "Stdout")),
      };
    },
    async listDir(path) {
      const { runtimeBridge: activeRuntimeBridge } = requireOpen();
      const events = activeRuntimeBridge.sendHostCommand({ ListDir: { path } });
      return {
        events,
        output: new TextDecoder().decode(extractStream(events, "Stdout")),
      };
    },
    async installPythonPackages({ requirements, options = {} }) {
      const {
        pyodide: activePyodide,
        module: activeModule,
      } = requireOpen();
      if (bundled.size === 0) {
        for (const fileName of loadBundledPackageNames(assetDir, activePyodide.FS ? activePyodide : activeModule)) {
          bundled.add(fileName);
        }
      }
      const reqs = typeof requirements === "string" ? [requirements] : requirements;
      return installPackages(reqs, activePyodide, {
        isBundled: (name) => bundled.has(name),
        allowedHosts,
        deps: options.deps,
      });
    },
    close() {
      if (!module && !runtimeBridge) {
        return;
      }
      bundled.clear();
      runtimeBridge?.close();
      if (module?._pyodide === pyodide) {
        delete module._pyodide;
      }
      if (module) {
        delete module._wasmshRuntimeBridge;
      }
      pyodide = null;
      runtimeBridge = null;
      module = null;
    },
  };
}
