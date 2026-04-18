import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

import pkg from "../../package.json" with { type: "json" };
import { createFullModule } from "../node-module.mjs";
import { createRuntimeBridge } from "../runtime-bridge.mjs";
import { buildEntropyContract, assertWorkspaceEmpty } from "./entropy-contract.mjs";
import { assertSerializableJsRefs, captureJsRefManifest } from "./jsref-registry.mjs";
import { buildSnapshotManifest, SNAPSHOT_ARTIFACT_LAYOUT, stableJsonStringify } from "./manifest.mjs";

function captureTableState(module) {
  const tableLength = Number(module.wasmTable?.length ?? 0);
  return {
    encoding: "json",
    size: tableLength,
    entries: Array.from({ length: tableLength }, (_, index) => ({
      index,
      kind: module.wasmTable?.get(index) ? "function" : "empty",
    })),
  };
}

async function runSelftest(runtimeBridge) {
  const echoEvents = runtimeBridge.sendHostCommand({ Run: { input: "echo hello" } });
  const pythonEvents = runtimeBridge.sendHostCommand({ Run: { input: "python3 -c \"print(1+1)\"" } });
  return {
    echo: echoEvents,
    python: pythonEvents,
  };
}

export async function buildSnapshot({
  assetDir,
  outputDir = null,
  compiledWasmModule = null,
  wasmBytes = null,
} = {}) {
  if (!assetDir) {
    throw new Error("assetDir is required");
  }

  const module = await createFullModule(assetDir, {
    makeSnapshot: true,
    compiledWasmModule,
    wasmBytes,
  });

  assertWorkspaceEmpty(module);

  const pyodide = module._pyodide;
  if (!pyodide?._api?.makeSnapshot) {
    throw new Error("Pyodide snapshot API is not available");
  }

  const memoryBytes = Uint8Array.from(pyodide._api.makeSnapshot());
  const runtimeBridge = createRuntimeBridge(module);
  const jsRefs = assertSerializableJsRefs(
    captureJsRefManifest({ assetDir, pyodideVersion: pyodide.version }),
  );
  const tableState = captureTableState(module);
  const selftest = await runSelftest(runtimeBridge);
  const entropy = buildEntropyContract();
  const versions = {
    pyodide: pyodide.version,
    emscripten: "embedded",
    loader: pkg.version,
    wasmsh: pkg.version,
  };
  const sbom = {
    format: "wasmsh-snapshot-sbom/v1",
    package: pkg.name,
    version: pkg.version,
    artifact_layout: SNAPSHOT_ARTIFACT_LAYOUT,
    entropy,
  };
  const manifest = buildSnapshotManifest({
    versions,
    memoryBytes,
    tableState,
    jsRefs,
    restoreStrategy: "pyodide-load-snapshot",
    selftest,
  });

  const artifact = {
    manifest,
    memoryBytes,
    jsRefs,
    tableState,
    selftest,
    sbom,
  };

  if (outputDir) {
    await mkdir(outputDir, { recursive: true });
    await writeFile(resolve(outputDir, "snapshot.manifest.json"), stableJsonStringify(manifest));
    await writeFile(resolve(outputDir, "memory.bin"), memoryBytes);
    await writeFile(resolve(outputDir, "jsrefs.json"), stableJsonStringify(jsRefs));
    await writeFile(resolve(outputDir, "table.json"), stableJsonStringify(tableState));
    await writeFile(resolve(outputDir, "selftest.json"), stableJsonStringify(selftest));
    await writeFile(resolve(outputDir, "sbom.json"), stableJsonStringify(sbom));
  }

  runtimeBridge.close();
  return artifact;
}
