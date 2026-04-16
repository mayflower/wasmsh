import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";

import { buildSnapshot } from "../../../packages/npm/wasmsh-pyodide/lib/snapshot/builder.mjs";

const assetDir = resolve(process.cwd(), "packages/npm/wasmsh-pyodide/assets");

test("snapshot builder writes the canonical artifact layout deterministically", async () => {
  const dirA = await mkdtemp(resolve(tmpdir(), "wasmsh-snapshot-a-"));
  const dirB = await mkdtemp(resolve(tmpdir(), "wasmsh-snapshot-b-"));

  try {
    const artifactA = await buildSnapshot({ assetDir, outputDir: dirA });
    const artifactB = await buildSnapshot({ assetDir, outputDir: dirB });

    const filesA = (await readdir(dirA)).sort();
    const filesB = (await readdir(dirB)).sort();
    assert.deepEqual(filesA, filesB);
    assert.deepEqual(filesA, [
      "jsrefs.json",
      "memory.bin.zst",
      "sbom.json",
      "selftest.json",
      "snapshot.manifest.json",
      "table.json",
    ]);

    assert.equal(artifactA.manifest.jsrefs_hash, artifactB.manifest.jsrefs_hash);
    assert.equal(artifactA.manifest.table_hash, artifactB.manifest.table_hash);
    assert.equal(artifactA.manifest.memory_size, artifactB.manifest.memory_size);
    assert.equal(typeof artifactA.manifest.memory_hash, "string");
    assert.equal(typeof artifactB.manifest.memory_hash, "string");
    assert.equal(typeof artifactA.manifest.snapshot_digest, "string");
    assert.equal(typeof artifactB.manifest.snapshot_digest, "string");

    const manifestText = await readFile(resolve(dirA, "snapshot.manifest.json"), "utf8");
    assert.match(manifestText, /snapshot_schema_version/);
    assert.ok(!manifestText.includes("latest"));
  } finally {
    await rm(dirA, { recursive: true, force: true });
    await rm(dirB, { recursive: true, force: true });
  }
});
