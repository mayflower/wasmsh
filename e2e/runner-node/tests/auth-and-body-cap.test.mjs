// Runner-server auth + body cap tests (audit F4).
//
// Bearer-token auth on /sessions* and a runner-side cap on POST body
// size. /healthz and /readyz stay open so liveness/readiness probes
// keep working without a credential.
import { test } from "node:test";
import assert from "node:assert/strict";

import { createRunnerServer } from "../../../tools/runner-node/src/server.mjs";

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

test("runner /healthz and /readyz stay open when auth is configured", async (t) => {
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-auth-health",
    authToken: "secret-runner-token",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const { port } = listening;
  try {
    const health = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(health.status, 200);
    const ready = await fetch(`http://127.0.0.1:${port}/readyz`);
    assert.equal(ready.status, 200);
  } finally {
    await service.close();
  }
});

test("runner /sessions without bearer token returns 401", async (t) => {
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-auth-missing",
    authToken: "secret-runner-token",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const { port } = listening;
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/sessions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ sessionId: "x" }),
    });
    assert.equal(resp.status, 401);
  } finally {
    await service.close();
  }
});

test("runner /sessions with wrong bearer token returns 401", async (t) => {
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-auth-wrong",
    authToken: "secret-runner-token",
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const { port } = listening;
  try {
    const resp = await fetch(`http://127.0.0.1:${port}/sessions`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: "Bearer wrong-runner-token",
      },
      body: JSON.stringify({ sessionId: "x" }),
    });
    assert.equal(resp.status, 401);
  } finally {
    await service.close();
  }
});

test("runner refuses oversized POST bodies with 413", async (t) => {
  // Set a tiny cap and POST a body that exceeds it. The handler should
  // never see the payload — readJson() raises before the request
  // is parsed.
  const service = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-body-cap",
    maxRequestBodyBytes: 256,
  });
  const listening = await listenOrSkip(t, service);
  if (!listening) {
    await service.close();
    return;
  }
  const { port } = listening;
  try {
    const huge = "A".repeat(1024);
    const resp = await fetch(`http://127.0.0.1:${port}/sessions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ sessionId: "x", extra: huge }),
    });
    assert.equal(resp.status, 413);
    const body = await resp.json();
    assert.equal(body.code, "WASMSH_REQUEST_BODY_TOO_LARGE");
  } finally {
    await service.close();
  }
});
