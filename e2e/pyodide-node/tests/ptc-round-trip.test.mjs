/**
 * E2E: PTC suspend/resume round-trip through the real Node host + Pyodide.
 *
 * Drives `runPtc` directly over stdio (the npm `RequestClient` does not yet
 * recognise the `host_call` / `ack` / `host_call_result` message types).
 * Verifies that:
 *   - the host emits a capability ack on boot;
 *   - `await tools.search(...)` inside user Python emits a `host_call`;
 *   - the host-side dispatcher's `host_call_result` is delivered to the
 *     awaiting Python coroutine;
 *   - the terminal envelope carries the trailing expression value as a
 *     native primitive (not double-`repr`'d).
 *
 * Gated on the presence of `pyodide.asm.wasm`; skips when the assets aren't
 * built in this checkout.
 */
import cp from "node:child_process";
import readline from "node:readline";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { describe, it } from "node:test";

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide");
const ASSETS_DIR = resolve(PKG_DIR, "assets");
const HOST_PATH = resolve(PKG_DIR, "node-host.mjs");
const HAS_ASSETS = existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

const skipUnlessAssets = HAS_ASSETS ? () => false : () => "pyodide assets not built";

describe("PTC round-trip via node-host runPtc", { skip: skipUnlessAssets() }, () => {
  it(
    "round-trips one tool call and returns the native value",
    { timeout: 120_000 },
    async () => {
      const child = cp.spawn(
        process.execPath,
        [HOST_PATH, "--asset-dir", ASSETS_DIR],
        { stdio: ["pipe", "pipe", "inherit"] },
      );
      const rl = readline.createInterface({
        input: child.stdout,
        crlfDelay: Infinity,
      });

      let ackSeen = false;
      let capabilities = null;
      const pendingById = new Map();
      const hostCalls = [];

      const writeMessage = (msg) => {
        child.stdin.write(JSON.stringify(msg) + "\n");
      };

      try {
        rl.on("line", (line) => {
          if (!line.trim()) return;
          let msg;
          try {
            msg = JSON.parse(line);
          } catch {
            return;
          }
          if (msg.type === "ack") {
            ackSeen = true;
            capabilities = msg.capabilities ?? null;
            return;
          }
          if (msg.type === "host_call") {
            hostCalls.push(msg);
            // Stub dispatcher: echo back "hit:<q>" for the search tool.
            const value =
              msg.tool === "search" ? `hit:${msg.args?.q ?? ""}` : null;
            writeMessage({
              type: "host_call_result",
              id: msg.id,
              ok: value !== null,
              value,
              error: value === null ? "UnknownTool" : undefined,
              message: value === null ? `no tool ${msg.tool}` : undefined,
            });
            return;
          }
          if (msg.id != null && pendingById.has(msg.id)) {
            const { resolve: ok, reject } = pendingById.get(msg.id);
            pendingById.delete(msg.id);
            if (msg.ok) ok(msg.result);
            else reject(new Error(msg.error ?? "unknown"));
          }
        });

        const send = (method, params) => {
          const id = pendingById.size + 1;
          return new Promise((resolve, reject) => {
            pendingById.set(id, { resolve, reject });
            writeMessage({ id, method, params });
          });
        };

        await send("init", {
          stepBudget: 0,
          initialFiles: [],
          allowedHosts: [],
        });
        assert.ok(ackSeen, "host should emit ack before init response");
        assert.deepEqual(
          capabilities,
          { host_call: "v1" },
          "host_call capability v1 must be advertised",
        );

        const result = await send("runPtc", {
          code: "await tools.search(q='foo')",
          tools: ["search"],
        });

        assert.ok(result?.envelope, "runPtc must return an envelope");
        const envelope = result.envelope;
        assert.equal(envelope.ok, true, `eval failed: ${JSON.stringify(envelope)}`);
        assert.equal(
          envelope.value,
          "hit:foo",
          "trailing expression must be native string, not repr-wrapped",
        );

        assert.equal(hostCalls.length, 1, "exactly one host_call should fire");
        assert.equal(hostCalls[0].tool, "search");
        assert.deepEqual(hostCalls[0].args, { q: "foo" });
      } finally {
        try {
          writeMessage({ id: 9999, method: "close", params: {} });
        } catch {
          /* ignore */
        }
        rl.close();
        child.stdin.end();
        await new Promise((r) => setTimeout(r, 200));
        if (!child.killed) child.kill();
      }
    },
  );

  it(
    "asyncio.gather fan-out correlates two parallel host_calls by id",
    { timeout: 120_000 },
    async () => {
      const child = cp.spawn(
        process.execPath,
        [HOST_PATH, "--asset-dir", ASSETS_DIR],
        { stdio: ["pipe", "pipe", "inherit"] },
      );
      const rl = readline.createInterface({
        input: child.stdout,
        crlfDelay: Infinity,
      });

      const pendingById = new Map();
      const seenHostCallIds = new Set();
      let ackSeen = false;

      const writeMessage = (msg) => {
        child.stdin.write(JSON.stringify(msg) + "\n");
      };

      try {
        rl.on("line", (line) => {
          if (!line.trim()) return;
          let msg;
          try {
            msg = JSON.parse(line);
          } catch {
            return;
          }
          if (msg.type === "ack") {
            ackSeen = true;
            return;
          }
          if (msg.type === "host_call") {
            seenHostCallIds.add(msg.id);
            writeMessage({
              type: "host_call_result",
              id: msg.id,
              ok: true,
              value: `hit:${msg.args?.q ?? ""}`,
            });
            return;
          }
          if (msg.id != null && pendingById.has(msg.id)) {
            const { resolve: ok, reject } = pendingById.get(msg.id);
            pendingById.delete(msg.id);
            if (msg.ok) ok(msg.result);
            else reject(new Error(msg.error ?? "unknown"));
          }
        });

        const send = (method, params) => {
          const id = pendingById.size + 1;
          return new Promise((resolve, reject) => {
            pendingById.set(id, { resolve, reject });
            writeMessage({ id, method, params });
          });
        };

        await send("init", { stepBudget: 0, initialFiles: [], allowedHosts: [] });
        assert.ok(ackSeen);

        const code = [
          "import asyncio",
          "results = await asyncio.gather(",
          "    tools.search(q='alpha'),",
          "    tools.search(q='beta'),",
          ")",
          "results",
        ].join("\n");

        const result = await send("runPtc", { code, tools: ["search"] });
        const envelope = result.envelope;
        assert.equal(envelope.ok, true, `eval failed: ${JSON.stringify(envelope)}`);
        assert.deepEqual(
          envelope.value,
          ["hit:alpha", "hit:beta"],
          "gather should return both per-call results in order",
        );
        assert.equal(seenHostCallIds.size, 2, "two distinct host_call ids");
      } finally {
        try {
          writeMessage({ id: 9999, method: "close", params: {} });
        } catch {
          /* ignore */
        }
        rl.close();
        child.stdin.end();
        await new Promise((r) => setTimeout(r, 200));
        if (!child.killed) child.kill();
      }
    },
  );
});
