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
}) {
  const module = await createRestoredModuleFromSnapshot(assetDir, snapshotBytes, { fetchHandlerSync });
  const runtimeBridge = createRuntimeBridge(module);
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
  const pyodide = module._pyodide;

  return {
    module,
    runtimeBridge,
    async run(command) {
      const pipResult = await handlePipCommand(command, pyodide, (opts) => this.installPythonPackages(opts));
      if (pipResult) {
        return pipResult;
      }
      return buildRunResult(runtimeBridge.sendHostCommand({ Run: { input: command } }));
    },
    async writeFile(path, content) {
      const events = runtimeBridge.sendHostCommand({
        WriteFile: {
          path,
          data: Array.from(content),
        },
      });
      return { events };
    },
    async readFile(path) {
      const events = runtimeBridge.sendHostCommand({ ReadFile: { path } });
      return {
        events,
        content: extractStream(events, "Stdout"),
        contentBase64: encodeBase64(extractStream(events, "Stdout")),
      };
    },
    async listDir(path) {
      const events = runtimeBridge.sendHostCommand({ ListDir: { path } });
      return {
        events,
        output: new TextDecoder().decode(extractStream(events, "Stdout")),
      };
    },
    async installPythonPackages({ requirements, options = {} }) {
      if (bundled.size === 0) {
        for (const fileName of loadBundledPackageNames(assetDir, pyodide.FS ? pyodide : module)) {
          bundled.add(fileName);
        }
      }
      const reqs = typeof requirements === "string" ? [requirements] : requirements;
      return installPackages(reqs, pyodide, {
        isBundled: (name) => bundled.has(name),
        allowedHosts,
        deps: options.deps,
      });
    },
    close() {
      runtimeBridge.close();
    },
  };
}
