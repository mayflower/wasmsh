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
