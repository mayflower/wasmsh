/**
 * Protocol parity tests: exercises all core HostCommand types through the
 * Pyodide runtime adapter and asserts the same WorkerEvent shapes as the
 * standalone browser build.
 *
 * Uses the host adapter (pyodide-host-adapter.mjs) to send commands,
 * mirroring how the standalone worker.js dispatches messages.
 *
 * Skip: SKIP_PYODIDE=1
 */
import { describe, it, before } from "node:test";
import assert from "node:assert/strict";

const SKIP = process.env.SKIP_PYODIDE === "1";

/** Decode a Stdout byte array to string. */
function decodeStdout(events) {
  const evt = events.find((e) => "Stdout" in e);
  if (!evt) return null;
  return new TextDecoder().decode(new Uint8Array(evt.Stdout));
}

describe("protocol parity: Pyodide runtime", () => {
  let adapter;

  before(async () => {
    if (SKIP) return;
    const mod = await import("../pyodide-host-adapter.mjs");
    adapter = await mod.createHostAdapter();
  });

  // ── Init ──────────────────────────────────────────────────

  it("Init returns Version event", { skip: SKIP, timeout: 30_000 }, async () => {
    const events = await adapter.send({ Init: { step_budget: 0 } });
    const ver = events.find((e) => "Version" in e);
    assert.ok(ver, "Init must return a Version event");
    assert.equal(ver.Version, "0.1.0");
  });

  // ── Run ───────────────────────────────────────────────────

  it("Run echo returns Stdout + Exit(0)", { skip: SKIP }, async () => {
    const events = await adapter.send({ Run: { input: "echo hello" } });
    assert.equal(decodeStdout(events), "hello\n");
    const exit = events.find((e) => "Exit" in e);
    assert.ok(exit);
    assert.equal(exit.Exit, 0);
  });

  // ── WriteFile ─────────────────────────────────────────────

  it("WriteFile returns FsChanged event", { skip: SKIP }, async () => {
    const events = await adapter.send({
      WriteFile: { path: "/workspace/parity.txt", data: Array.from(new TextEncoder().encode("parity")) },
    });
    const fc = events.find((e) => "FsChanged" in e);
    assert.ok(fc, "WriteFile must return FsChanged");
    assert.equal(fc.FsChanged, "/workspace/parity.txt");
  });

  // ── ReadFile ──────────────────────────────────────────────

  it("ReadFile returns Stdout with file content", { skip: SKIP }, async () => {
    const events = await adapter.send({
      ReadFile: { path: "/workspace/parity.txt" },
    });
    assert.equal(decodeStdout(events), "parity");
  });

  // ── ListDir ───────────────────────────────────────────────

  it("ListDir includes written file", { skip: SKIP }, async () => {
    const events = await adapter.send({
      ListDir: { path: "/workspace" },
    });
    const listing = decodeStdout(events);
    assert.ok(listing, "ListDir must return Stdout");
    assert.ok(listing.includes("parity.txt"), "listing: " + listing);
  });

  // ── Run cat reads WriteFile content ───────────────────────

  it("Run cat reads file written via WriteFile", { skip: SKIP }, async () => {
    const events = await adapter.send({
      Run: { input: "cat /workspace/parity.txt" },
    });
    assert.equal(decodeStdout(events), "parity");
    const exit = events.find((e) => "Exit" in e);
    assert.equal(exit.Exit, 0);
  });

  // ── Cancel ────────────────────────────────────────────────

  it("Cancel returns Diagnostic(Info) event", { skip: SKIP }, async () => {
    const events = await adapter.send("Cancel");
    const diag = events.find((e) => "Diagnostic" in e);
    assert.ok(diag, "Cancel must return a Diagnostic event");
    assert.equal(diag.Diagnostic[0], "Info");
    assert.ok(diag.Diagnostic[1].includes("cancel"));
  });

  // ── Step budget ───────────────────────────────────────────

  it("step budget kills infinite loop with non-zero exit", { skip: SKIP, timeout: 30_000 }, async () => {
    // Create a separate runtime with a small budget
    const mod2 = await import("../pyodide-host-adapter.mjs");
    const adapter2 = await mod2.createHostAdapter({ stepBudget: 200 });

    const events = await adapter2.send({
      Run: { input: "while true; do :; done" },
    });
    const exit = events.find((e) => "Exit" in e);
    assert.ok(exit, "must return Exit event");
    assert.notEqual(exit.Exit, 0, "resource-exhausted exit must be non-zero");

    adapter2.destroy();
  });
});
