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

test("add overwrites an existing session with the same id", () => {
  const registry = createSessionRegistry();
  registry.add({ id: "x", workerId: "first" });
  registry.add({ id: "x", workerId: "second" });
  assert.equal(registry.values().length, 1);
  assert.equal(registry.get("x").workerId, "second");
});
