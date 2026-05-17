import { test } from "node:test";
import assert from "node:assert/strict";

import { createSessionRegistry } from "../../../tools/runner-node/src/session-registry.mjs";

function fakeSession(id) {
  return { id, workerId: `worker-${id}` };
}

test("get returns null for an unknown session id", () => {
  const registry = createSessionRegistry();
  assert.equal(registry.get("missing"), null);
});

test("add then list returns shallow id/workerId entries", () => {
  const registry = createSessionRegistry();
  registry.add(fakeSession("a"));
  registry.add(fakeSession("b"));
  const listed = registry.list();
  assert.deepEqual(listed, [
    { id: "a", workerId: "worker-a" },
    { id: "b", workerId: "worker-b" },
  ]);
});

test("delete removes the session and subsequent get returns null", () => {
  const registry = createSessionRegistry();
  const session = fakeSession("x");
  registry.add(session);
  registry.delete("x");
  assert.equal(registry.get("x"), null);
  assert.deepEqual(registry.list(), []);
});

test("values yields the original session objects in insertion order", () => {
  const registry = createSessionRegistry();
  const a = fakeSession("a");
  const b = fakeSession("b");
  registry.add(a);
  registry.add(b);
  const values = registry.values();
  assert.strictEqual(values[0], a);
  assert.strictEqual(values[1], b);
});

test("add rejects a second session with an existing id (D1)", () => {
  // F-series D1 changed SessionRegistry.add from "silently overwrite" to
  // "reject duplicates" so caller-supplied IDs can't squat or hijack
  // another session. The previous behavior is unsafe under any auth
  // model that lets a client choose its session id.
  const registry = createSessionRegistry();
  registry.add({ id: "x", workerId: "first" });
  assert.throws(
    () => registry.add({ id: "x", workerId: "second" }),
    (e) => e.code === "WASMSH_SESSION_EXISTS",
  );
  // The first session is still there and unchanged.
  assert.equal(registry.values().length, 1);
  assert.equal(registry.get("x").workerId, "first");
});
