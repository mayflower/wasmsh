/**
 * Tests the wasmsh-pyodide npm package session API (createNodeSession).
 *
 * These exercise the package-level API rather than the raw C ABI.
 * They require built Pyodide assets in packages/npm/wasmsh-pyodide/assets/.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import { createSessionTracker } from "./test-session-helper.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");

const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

let createNodeSession;
if (!SKIP) {
  ({ createNodeSession } = await import(resolve(PKG_DIR, "index.js")));
}

describe("createNodeSession API", () => {
  const openSession = SKIP ? null : createSessionTracker(createNodeSession, ASSETS_DIR);

  it("returns a session with expected methods", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    assert.equal(typeof session.run, "function");
    assert.equal(typeof session.writeFile, "function");
    assert.equal(typeof session.readFile, "function");
    assert.equal(typeof session.listDir, "function");
    assert.equal(typeof session.close, "function");
  });

  it("bash echo returns stdout and exitCode 0", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    const result = await session.run("echo hello");
    assert.equal(result.stdout.trim(), "hello");
    assert.equal(result.exitCode, 0);
    assert.equal(result.stderr, "");
  });

  it("python3 -c print(42) returns stdout", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    const result = await session.run("python3 -c \"print(42)\"");
    assert.equal(result.stdout.trim(), "42");
    assert.equal(result.exitCode, 0);
  });

  it("bash writes file, python reads it", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();

    const write = await session.run("echo 'shared-data' > /workspace/shared.txt");
    assert.equal(write.exitCode, 0);

    const read = await session.run(
      "python3 -c \"print(open('/workspace/shared.txt').read().strip())\"",
    );
    assert.equal(read.stdout.trim(), "shared-data");
    assert.equal(read.exitCode, 0);
  });

  it("python writes file, bash reads it", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();

    const write = await session.run(
      "python3 -c \"open('/workspace/pyfile.txt','w').write('from-python')\"",
    );
    assert.equal(write.exitCode, 0);

    const read = await session.run("cat /workspace/pyfile.txt");
    assert.equal(read.stdout.trim(), "from-python");
  });

  it("writeFile + readFile roundtrips binary data", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    const original = new Uint8Array([0x00, 0x42, 0xff, 0x80, 0x01]);

    await session.writeFile("/workspace/bin.dat", original);
    const result = await session.readFile("/workspace/bin.dat");

    assert.deepEqual(result.content, original);
  });

  it("writeFile + listDir shows the file", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    await session.writeFile(
      "/workspace/listed.txt",
      new TextEncoder().encode("test"),
    );

    const dir = await session.listDir("/workspace");
    assert.ok(
      dir.output.includes("listed.txt"),
      `expected 'listed.txt' in listDir output: ${dir.output}`,
    );
  });

  it("session.close terminates cleanly", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    // Don't add to sessions array since we close it explicitly
    await session.close();
    // After close, calling run should reject
    await assert.rejects(
      () => session.run("echo after-close"),
      "should reject after close",
    );
  });

  it("initialFiles are accessible at session start", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession({
      initialFiles: [
        { path: "/workspace/seed.txt", content: new TextEncoder().encode("seeded") },
      ],
    });

    const result = await session.run("cat /workspace/seed.txt");
    assert.equal(result.stdout.trim(), "seeded");
  });

  it("non-zero exit code is returned", { skip: SKIP, timeout: 60_000 }, async () => {
    const session = await openSession();
    const result = await session.run("exit 42");
    assert.equal(result.exitCode, 42);
  });
});
