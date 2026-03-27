/**
 * Test B: Python writes a file, shell reads it.
 *
 * Python writes to /workspace, then the wasmsh runtime reads via cat.
 * Proves bidirectional shared workspace.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

describe("workspace: Python → shell", () => {
  it("shell cat reads a file written by Python", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createFullModule } = await import("../host-wrapper.mjs");
    const mod = await createFullModule();

    // Python writes /workspace/b.txt
    const pyResult = mod.ccall("PyRun_SimpleString", "number",
      ["string"],
      [`open("/workspace/b.txt", "w").write("python-data")`]);
    assert.equal(pyResult, 0, "Python write failed");

    // Create a wasmsh runtime
    const handle = mod.ccall("wasmsh_runtime_new", "number", [], []);
    assert.ok(handle !== 0);

    // Init
    const initCmd = JSON.stringify({ Init: { step_budget: 0 } });
    const initPtr = mod.stringToNewUTF8(initCmd);
    const initResPtr = mod.ccall("wasmsh_runtime_handle_json", "number",
      ["number", "number"], [handle, initPtr]);
    mod._free(initPtr);
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [initResPtr]);

    // Shell: cat /workspace/b.txt
    const runCmd = JSON.stringify({ Run: { input: "cat /workspace/b.txt" } });
    const runPtr = mod.stringToNewUTF8(runCmd);
    const runResPtr = mod.ccall("wasmsh_runtime_handle_json", "number",
      ["number", "number"], [handle, runPtr]);
    mod._free(runPtr);
    const runResult = JSON.parse(mod.UTF8ToString(runResPtr));
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [runResPtr]);

    // Check stdout contains the Python-written content
    const stdoutEvt = runResult.find((e) => "Stdout" in e);
    assert.ok(stdoutEvt, "cat should produce Stdout");
    const stdoutText = new TextDecoder().decode(new Uint8Array(stdoutEvt.Stdout));
    assert.equal(stdoutText, "python-data");

    // Check exit 0
    const exitEvt = runResult.find((e) => "Exit" in e);
    assert.ok(exitEvt);
    assert.equal(exitEvt.Exit, 0, "cat should exit 0");

    mod.ccall("wasmsh_runtime_free", null, ["number"], [handle]);
  });
});
