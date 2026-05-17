/**
 * E2E: the npm `RequestClient.runPtc` surface drives the protocol end-to-end.
 *
 * Complements `ptc-round-trip.test.mjs` (which drives stdio directly): this
 * test exercises the high-level client method that npm consumers (incl.
 * `@langchain/wasmsh` in deepagentsjs) actually use. Gated on Pyodide assets.
 */
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");
const HAS_ASSETS = existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

const { createNodeSession } = await import(resolve(PKG_DIR, "index.js"));

describe("RequestClient.runPtc", { skip: !HAS_ASSETS && "pyodide assets not built" }, () => {
  it("round-trips one tool call via onHostCall", { timeout: 120_000 }, async () => {
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    try {
      assert.equal(session.capabilities.host_call, "v1");
      const calls = [];
      const envelope = await session.runPtc({
        code: "await tools.search(q='foo')",
        tools: ["search"],
        onHostCall: async (call) => {
          calls.push(call);
          assert.equal(call.tool, "search");
          return { ok: true, value: `hit:${call.args.q}` };
        },
      });
      assert.equal(envelope.ok, true, JSON.stringify(envelope));
      assert.equal(envelope.value, "hit:foo");
      assert.equal(calls.length, 1);
    } finally {
      await session.close();
    }
  });

  it("propagates onHostCall errors as host_call_result failures", { timeout: 120_000 }, async () => {
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    try {
      const code = [
        "result = None",
        "try:",
        "    await tools.boom()",
        "except Exception as exc:",
        "    result = str(exc)",
        "result",
      ].join("\n");
      const envelope = await session.runPtc({
        code,
        tools: ["boom"],
        onHostCall: async () => {
          const err = new Error("tool exploded");
          err.name = "BoomError";
          throw err;
        },
      });
      assert.equal(envelope.ok, true, JSON.stringify(envelope));
      assert.match(envelope.value, /tool exploded/);
    } finally {
      await session.close();
    }
  });

  it("rejects when host_call capability is missing", { timeout: 30_000 }, async () => {
    // Build a stand-in that pretends the host advertised nothing.
    const session = await createNodeSession({ assetDir: ASSETS_DIR });
    try {
      session._capabilities.host_call = undefined;
      await assert.rejects(
        () =>
          session.runPtc({
            code: "1+1",
            tools: [],
            onHostCall: async () => ({ ok: true, value: null }),
          }),
        /host_call capability/,
      );
    } finally {
      await session.close();
    }
  });
});
