import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunnerServer } from "../../../tools/runner-node/src/server.mjs";

async function jsonRequest(baseUrl, path, { method = "GET", body } = {}) {
  const response = await fetch(`${baseUrl}${path}`, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  const payload = await response.json();
  return { response, payload };
}

async function listenOrSkip(t, service) {
  try {
    return await service.listen();
  } catch (error) {
    if (error?.code === "EPERM") {
      t.skip("local TCP listen is not permitted in this environment");
      return null;
    }
    throw error;
  }
}

test("runner server exposes the full session api over HTTP", async (t) => {
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-http-test",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const { port } = listening;
  const baseUrl = `http://127.0.0.1:${port}`;

  try {
    const create = await jsonRequest(baseUrl, "/sessions", {
      method: "POST",
      body: {
        sessionId: "http-session",
        initialFiles: [
          {
            path: "/workspace/input.txt",
            contentBase64: Buffer.from("seeded").toString("base64"),
          },
        ],
      },
    });
    assert.equal(create.response.status, 201);
    assert.equal(create.payload.session.sessionId, "http-session");

    const init = await jsonRequest(baseUrl, "/sessions/http-session/init", {
      method: "POST",
      body: {},
    });
    assert.equal(init.response.status, 200);
    assert.equal(init.payload.result.sessionId, "http-session");

    const read = await jsonRequest(baseUrl, "/sessions/http-session/read-file", {
      method: "POST",
      body: { path: "/workspace/input.txt" },
    });
    assert.equal(
      Buffer.from(read.payload.result.contentBase64, "base64").toString("utf8"),
      "seeded",
    );

    const write = await jsonRequest(baseUrl, "/sessions/http-session/write-file", {
      method: "POST",
      body: {
        path: "/workspace/out.txt",
        contentBase64: Buffer.from("hello").toString("base64"),
      },
    });
    assert.equal(write.response.status, 200);

    const run = await jsonRequest(baseUrl, "/sessions/http-session/run", {
      method: "POST",
      body: { command: "cat /workspace/out.txt" },
    });
    assert.equal(run.payload.result.exitCode, 0);
    assert.match(run.payload.result.stdout, /hello/);

    const list = await jsonRequest(baseUrl, "/sessions/http-session/list-dir", {
      method: "POST",
      body: { path: "/workspace" },
    });
    assert.match(list.payload.result.output, /input\.txt/);
    assert.match(list.payload.result.output, /out\.txt/);

    const close = await jsonRequest(baseUrl, "/sessions/http-session", {
      method: "DELETE",
    });
    assert.equal(close.payload.result.closed, true);

    const missing = await jsonRequest(baseUrl, "/sessions/http-session/run", {
      method: "POST",
      body: { command: "echo nope" },
    });
    assert.equal(missing.response.status, 404);
  } finally {
    await service.close();
  }
});

test("run accepts a per-command timeout and enforces it server-side", async (t) => {
  // A client socket timeout alone would abandon the request while the
  // command kept running, leaving the session pinned and the caller unable
  // to tell a slow command from a dead runner. The deadline belongs here,
  // where the worker can actually be terminated.
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-timeout-test",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const baseUrl = `http://127.0.0.1:${listening.port}`;

  try {
    const create = await jsonRequest(baseUrl, "/sessions", {
      method: "POST",
      body: { sessionId: "timeout-session" },
    });
    assert.equal(create.response.status, 201);

    const started = Date.now();
    const run = await jsonRequest(baseUrl, "/sessions/timeout-session/run", {
      method: "POST",
      body: {
        command: 'python3 -c "import time; time.sleep(30)"',
        timeoutMs: 2000,
      },
    });
    const elapsedMs = Date.now() - started;

    assert.equal(run.response.status, 200);
    assert.equal(run.payload.result.timedOut, true);
    // GNU timeout(1)'s convention, so an agent can branch on it.
    assert.equal(run.payload.result.exitCode, 124);
    assert.match(run.payload.result.output, /timed out after 2s/);
    // The 30-second command did not run to completion.
    assert.ok(elapsedMs < 20_000, `expected an early return, took ${elapsedMs}ms`);

    // The worker was terminated, so the session is gone rather than left
    // serving an interpreter mid-abandoned-evaluation.
    const after = await jsonRequest(baseUrl, "/sessions/timeout-session/run", {
      method: "POST",
      body: { command: "echo still here" },
    });
    assert.equal(after.response.status, 404);
  } finally {
    await service.close();
  }
});

test("run rejects a non-positive or absurd timeout instead of honoring it", async (t) => {
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-timeout-clamp-test",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const baseUrl = `http://127.0.0.1:${listening.port}`;

  try {
    await jsonRequest(baseUrl, "/sessions", {
      method: "POST",
      body: { sessionId: "clamp-session" },
    });

    // Zero, negative, and non-numeric values mean "no explicit deadline"
    // and fall back to the runner's own ceiling — they must not be passed
    // through as an instant timeout.
    for (const timeoutMs of [0, -1, "soon", null]) {
      const run = await jsonRequest(baseUrl, "/sessions/clamp-session/run", {
        method: "POST",
        body: { command: "echo fine", timeoutMs },
      });
      assert.equal(run.response.status, 200, `timeoutMs=${timeoutMs}`);
      assert.equal(run.payload.result.timedOut, undefined);
      assert.match(run.payload.result.output, /fine/);
    }
  } finally {
    await service.close();
  }
});
