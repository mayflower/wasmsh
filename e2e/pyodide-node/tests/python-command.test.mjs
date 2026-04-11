/**
 * Tests for python/python3 as in-process shell commands inside the
 * Pyodide-backed wasmsh runtime.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

import { extractStream } from "../../../packages/npm/wasmsh-pyodide/lib/protocol.mjs";

const SKIP = process.env.SKIP_PYODIDE === "1";

function decodeStdout(events) {
  const bytes = extractStream(events, "Stdout");
  if (bytes.length === 0) return null;
  return new TextDecoder().decode(bytes);
}

function decodeStderr(events) {
  const bytes = extractStream(events, "Stderr");
  if (bytes.length === 0) return null;
  return new TextDecoder().decode(bytes);
}

describe("python command in wasmsh", () => {
  let adapter;

  before(async () => {
    if (SKIP) return;
    const mod = await import("../pyodide-host-adapter.mjs");
    adapter = await mod.createHostAdapter({ fullPython: true });
  });

  // ── python3 -c ────────────────────────────────────────────

  it("python3 -c 'print(40+2)' produces stdout 42", { skip: SKIP, timeout: 30_000 }, async () => {
    const events = await adapter.send({
      Run: { input: "python3 -c 'print(40+2)'" },
    });
    assert.equal(decodeStdout(events), "42\n");
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0);
  });

  // ── heredoc-fed Python script ─────────────────────────────

  it("python3 reads stdin from heredoc", { skip: SKIP, timeout: 30_000 }, async () => {
    const events = await adapter.send({
      Run: { input: `python3 <<'EOF'
import sys
print("hello from heredoc")
EOF` },
    });
    assert.equal(decodeStdout(events), "hello from heredoc\n");
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0);
  });

  // ── Python writes a file that shell reads ─────────────────

  it("Python writes a file, shell reads it immediately", { skip: SKIP, timeout: 30_000 }, async () => {
    const events = await adapter.send({
      Run: {
        input: `python3 -c 'open("/workspace/from_py_cmd.txt","w").write("py-wrote-this")' && cat /workspace/from_py_cmd.txt`,
      },
    });
    assert.equal(decodeStdout(events), "py-wrote-this");
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0);
  });

  // ── Python stderr surfaces as worker Stderr events ────────

  it("python3 stderr surfaces in worker events", { skip: SKIP, timeout: 30_000 }, async () => {
    const events = await adapter.send({
      Run: { input: "python3 -c 'import sys; print(\"err-msg\", file=sys.stderr)'" },
    });
    const stderr = decodeStderr(events);
    assert.ok(stderr, "should have stderr output");
    assert.ok(stderr.includes("err-msg"), "stderr: " + stderr);
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0);
  });

  // ── sqlite3 stdlib: create, insert, query ────────────────
  // Regression test for a missing sqlite3 wheel in bundled assets.
  // sqlite3 is an unvendored cpython_module in Pyodide 0.28+; the
  // runtime must ship the wheel or `import sqlite3` fails offline.

  it("python3 can use sqlite3 stdlib end-to-end", { skip: SKIP, timeout: 60_000 }, async () => {
    const script = `python3 <<'EOF'
import sqlite3
conn = sqlite3.connect(":memory:")
cur = conn.cursor()
cur.execute("CREATE TABLE t (id INTEGER, name TEXT)")
cur.executemany("INSERT INTO t VALUES (?,?)", [(1,"a"),(2,"b"),(3,"c")])
conn.commit()
cur.execute("SELECT COUNT(*) FROM t")
print("count:", cur.fetchone()[0])
cur.execute("SELECT name FROM t ORDER BY id DESC LIMIT 1")
print("last:", cur.fetchone()[0])
conn.close()
EOF`;
    const events = await adapter.send({ Run: { input: script } });
    const stdout = decodeStdout(events);
    const stderr = decodeStderr(events);
    assert.ok(
      stdout?.includes("count: 3") && stdout?.includes("last: c"),
      `stdout: ${stdout} | stderr: ${stderr}`,
    );
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0, `stderr: ${stderr}`);
  });
});
