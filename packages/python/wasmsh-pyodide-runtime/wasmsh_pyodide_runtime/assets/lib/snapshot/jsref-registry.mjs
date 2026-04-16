import { digestJson } from "./manifest.mjs";

function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function captureJsRefManifest({ assetDir, pyodideVersion }) {
  return [
    {
      kind: "loader-entrypoint",
      id: "pyodide.asm.js",
      path: `${assetDir}/pyodide.asm.js`,
    },
    {
      kind: "loader-entrypoint",
      id: "pyodide.mjs",
      path: `${assetDir}/pyodide.mjs`,
    },
    {
      kind: "pyodide-version",
      id: pyodideVersion,
    },
  ];
}

export function assertSerializableJsRefs(jsRefs) {
  if (!Array.isArray(jsRefs) || jsRefs.length === 0) {
    throw new Error("jsrefs manifest must contain deterministic descriptors");
  }
  for (const entry of jsRefs) {
    if (!isPlainObject(entry)) {
      throw new Error("jsrefs entries must be plain objects");
    }
    if (typeof entry.kind !== "string" || typeof entry.id !== "string") {
      throw new Error("jsrefs entries must expose string kind/id fields");
    }
    for (const value of Object.values(entry)) {
      if (
        value !== null
        && typeof value !== "string"
        && typeof value !== "number"
        && typeof value !== "boolean"
      ) {
        throw new Error("jsrefs entries must only contain deterministic scalar values");
      }
    }
  }
  return jsRefs;
}

export function hashJsRefs(jsRefs) {
  return digestJson(jsRefs);
}
