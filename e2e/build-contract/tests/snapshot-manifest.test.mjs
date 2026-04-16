import { test } from "node:test";
import assert from "node:assert/strict";

import {
  buildSnapshotManifest,
  SNAPSHOT_ARTIFACT_LAYOUT,
} from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/manifest.mjs";

test("snapshot manifest contains the required versioned contract fields", () => {
  const manifest = buildSnapshotManifest({
    versions: {
      pyodide: "0.29.3",
      emscripten: "embedded",
      loader: "0.5.10",
      wasmsh: "0.5.10",
    },
    memoryBytes: new Uint8Array([1, 2, 3]),
    tableState: { size: 0, entries: [] },
    jsRefs: [{ kind: "loader-entrypoint", id: "pyodide.asm.js" }],
    restoreStrategy: "pyodide-load-snapshot",
    selftest: { ok: true },
  });

  assert.equal(manifest.snapshot_schema_version, 1);
  assert.equal(manifest.versions.pyodide, "0.29.3");
  assert.equal(manifest.versions.loader, "0.5.10");
  assert.equal(manifest.memory_size, 3);
  assert.equal(typeof manifest.memory_hash, "string");
  assert.equal(typeof manifest.table_hash, "string");
  assert.equal(typeof manifest.jsrefs_hash, "string");
  assert.equal(typeof manifest.snapshot_digest, "string");
  assert.equal(manifest.restore_strategy, "pyodide-load-snapshot");
  assert.deepEqual(manifest.artifact_layout, SNAPSHOT_ARTIFACT_LAYOUT);
});
