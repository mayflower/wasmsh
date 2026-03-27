/**
 * Test A: Rust writes a file, Python reads it.
 *
 * Proves that Rust code and Python code see the same live Emscripten
 * filesystem inside a single custom Pyodide module.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

describe("shared FS: Rust → Python", () => {
  it("Python reads a file written by Rust", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createFullModule } = await import("../host-wrapper.mjs");
    const mod = await createFullModule();

    // Rust writes /workspace/from_rust.txt via Emscripten's POSIX FS
    const path = mod.stringToNewUTF8("/workspace/from_rust.txt");
    const text = mod.stringToNewUTF8("hello from rust");
    const rc = mod.ccall("wasmsh_probe_write_text", "number",
      ["number", "number"], [path, text]);
    mod._free(path);
    mod._free(text);
    assert.equal(rc, 0, "wasmsh_probe_write_text returned non-zero");

    // Python reads the same file
    const pyResult = mod.ccall("PyRun_SimpleString", "number",
      ["string"],
      [`
import pathlib
content = pathlib.Path("/workspace/from_rust.txt").read_text()
assert content == "hello from rust", f"got: {content!r}"
pathlib.Path("/tmp/_test_result.txt").write_text(content)
`]);
    assert.equal(pyResult, 0, "Python script failed");

    // Double-check: read /tmp/_test_result.txt via FS API
    const result = new TextDecoder().decode(mod.FS.readFile("/tmp/_test_result.txt"));
    assert.equal(result, "hello from rust");
  });
});
