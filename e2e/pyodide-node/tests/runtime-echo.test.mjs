/**
 * Verifies the real wasmsh runtime (not just probe functions) works
 * inside the custom Pyodide module via C ABI → JSON protocol.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

describe("wasmsh runtime in Pyodide", () => {
  it("init + echo hello returns stdout and exit 0", { skip: SKIP, timeout: 30_000 }, async () => {
    const { createProbeModule } = await import("../host-wrapper.mjs");
    const mod = await createProbeModule();

    // Create a runtime instance
    const handle = mod.ccall("wasmsh_runtime_new", "number", [], []);
    assert.ok(handle !== 0, "wasmsh_runtime_new returned null");

    // Init with unlimited budget
    const initCmd = JSON.stringify({ Init: { step_budget: 0 } });
    const initCmdPtr = mod.stringToNewUTF8(initCmd);
    const initResultPtr = mod.ccall("wasmsh_runtime_handle_json", "number",
      ["number", "number"], [handle, initCmdPtr]);
    mod._free(initCmdPtr);

    const initResult = mod.UTF8ToString(initResultPtr);
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [initResultPtr]);

    const initEvents = JSON.parse(initResult);
    const versionEvt = initEvents.find((e) => "Version" in e);
    assert.ok(versionEvt, "Init should return a Version event");
    assert.equal(versionEvt.Version, "0.1.0");

    // Run "echo hello"
    const runCmd = JSON.stringify({ Run: { input: "echo hello" } });
    const runCmdPtr = mod.stringToNewUTF8(runCmd);
    const runResultPtr = mod.ccall("wasmsh_runtime_handle_json", "number",
      ["number", "number"], [handle, runCmdPtr]);
    mod._free(runCmdPtr);

    const runResult = mod.UTF8ToString(runResultPtr);
    mod.ccall("wasmsh_runtime_free_string", null, ["number"], [runResultPtr]);

    const runEvents = JSON.parse(runResult);

    // Check stdout
    const stdoutEvt = runEvents.find((e) => "Stdout" in e);
    assert.ok(stdoutEvt, "Run should return a Stdout event");
    const stdoutText = new TextDecoder().decode(new Uint8Array(stdoutEvt.Stdout));
    assert.equal(stdoutText, "hello\n");

    // Check exit 0
    const exitEvt = runEvents.find((e) => "Exit" in e);
    assert.ok(exitEvt, "Run should return an Exit event");
    assert.equal(exitEvt.Exit, 0);

    // Free the runtime
    mod.ccall("wasmsh_runtime_free", null, ["number"], [handle]);
  });
});
