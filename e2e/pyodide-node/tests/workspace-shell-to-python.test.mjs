/**
 * Test A: Shell writes a file, Python reads it.
 *
 * Uses the real wasmsh runtime (not probe functions) to run a shell
 * command that writes to /workspace, then Python reads the same path.
 * Proves the shell and Python share the same live filesystem.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

describe("workspace: shell → Python", () => {
  it("Python reads a file written by shell echo", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createFullModule } = await import("../host-wrapper.mjs");
    const mod = await createFullModule();

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

    // Shell: echo shell-data > /workspace/a.txt
    const runCmd = JSON.stringify({ Run: { input: "echo shell-data > /workspace/a.txt" } });
    const runPtr = mod.stringToNewUTF8(runCmd);
    const runResPtr = mod.ccall("wasmsh_runtime_handle_json", "number",
      ["number", "number"], [handle, runPtr]);
    mod._free(runPtr);
    const runResult = JSON.parse(mod.UTF8ToString(runResPtr));
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [runResPtr]);

    const exitEvt = runResult.find((e) => "Exit" in e);
    assert.ok(exitEvt, "shell command should return Exit");
    assert.equal(exitEvt.Exit, 0, "shell echo should exit 0");

    // Python reads the same file from the same /workspace
    const pyResult = mod.ccall("PyRun_SimpleString", "number",
      ["string"],
      [`
content = open("/workspace/a.txt").read()
assert content == "shell-data\\n", f"got: {content!r}"
open("/tmp/_ws_result.txt", "w").write(content)
`]);
    assert.equal(pyResult, 0, "Python script failed");

    // Verify via FS API
    const result = new TextDecoder().decode(mod.FS.readFile("/tmp/_ws_result.txt"));
    assert.equal(result, "shell-data\n");

    mod.ccall("wasmsh_runtime_free", null, ["number"], [handle]);
  });
});
