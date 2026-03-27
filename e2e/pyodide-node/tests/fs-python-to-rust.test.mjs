/**
 * Test B: Python writes a file, Rust reads and verifies it.
 *
 * Proves shared filesystem in the Python → Rust direction.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

describe("shared FS: Python → Rust", () => {
  it("Rust verifies a file written by Python", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createFullModule } = await import("../host-wrapper.mjs");
    const mod = await createFullModule();

    // Python writes /workspace/from_python.txt
    const pyResult = mod.ccall("PyRun_SimpleString", "number",
      ["string"],
      [`
import pathlib
pathlib.Path("/workspace/from_python.txt").write_text("hello from python")
`]);
    assert.equal(pyResult, 0, "Python script failed");

    // Rust reads and verifies the same file
    const path = mod.stringToNewUTF8("/workspace/from_python.txt");
    const expected = mod.stringToNewUTF8("hello from python");
    const rc = mod.ccall("wasmsh_probe_file_equals", "number",
      ["number", "number"], [path, expected]);
    mod._free(path);
    mod._free(expected);
    assert.equal(rc, 1, "wasmsh_probe_file_equals returned 0 (content mismatch)");
  });
});
