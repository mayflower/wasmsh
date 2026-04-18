import { createHash } from "node:crypto";

export const SNAPSHOT_SCHEMA_VERSION = 1;
export const SNAPSHOT_ARTIFACT_LAYOUT = Object.freeze([
  "snapshot.manifest.json",
  "memory.bin",
  "jsrefs.json",
  "table.json",
  "selftest.json",
  "sbom.json",
]);

export function stableJsonStringify(value) {
  return JSON.stringify(sortValue(value));
}

function sortValue(value) {
  if (Array.isArray(value)) {
    return value.map(sortValue);
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, sortValue(value[key])]),
    );
  }
  return value;
}

export function digestBytes(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function digestJson(value) {
  return createHash("sha256").update(stableJsonStringify(value)).digest("hex");
}

export function buildSnapshotManifest({
  versions,
  memoryBytes,
  tableState,
  jsRefs,
  restoreStrategy,
  selftest,
}) {
  const jsrefs_hash = digestJson(jsRefs);
  const table_hash = digestJson(tableState);
  const memory_hash = digestBytes(memoryBytes);
  const memory_size = memoryBytes.byteLength ?? memoryBytes.length;
  const table_size = tableState.size ?? tableState.entries?.length ?? 0;
  const selftest_hash = digestJson(selftest);
  const snapshot_digest = digestJson({
    memory_hash,
    table_hash,
    jsrefs_hash,
    restoreStrategy,
    versions,
  });

  return {
    snapshot_schema_version: SNAPSHOT_SCHEMA_VERSION,
    versions,
    memory_size,
    memory_hash,
    table_size,
    table_hash,
    jsrefs_hash,
    snapshot_digest,
    restore_strategy: restoreStrategy,
    artifact_layout: SNAPSHOT_ARTIFACT_LAYOUT,
    selftest_hash,
  };
}
