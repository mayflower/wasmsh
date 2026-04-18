import { spawn } from "node:child_process";
import http from "node:http";
import { once } from "node:events";
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

async function reservePort() {
  const server = http.createServer();
  try {
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
  } catch (error) {
    server.close();
    throw error;
  }
  const { port } = server.address();
  await new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
  return port;
}

async function waitForReady(baseUrl, timeoutMs = 60_000) {
  const startedAt = Date.now();
  while (Date.now() - startedAt < timeoutMs) {
    try {
      const response = await fetch(`${baseUrl}/readyz`);
      if (response.ok) {
        const payload = await response.json();
        if (payload.ready) {
          return;
        }
      }
    } catch {
      // keep polling while the process comes up
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`dispatcher did not become ready within ${timeoutMs}ms`);
}

async function stopProcess(child) {
  if (child.exitCode !== null) {
    return;
  }
  child.kill("SIGTERM");
  await once(child, "exit");
}

test("dispatcher exposes the session api with runner affinity over HTTP", { timeout: 180_000 }, async (t) => {
  const runnerA = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-a",
    restoreSlots: 2,
  });
  const runnerB = await createRunnerServer({
    port: 0,
    host: "127.0.0.1",
    runnerId: "runner-b",
    restoreSlots: 2,
  });
  const runners = [];
  let dispatcher;
  try {
    let dispatcherPort;
    try {
      dispatcherPort = await reservePort();
    } catch (error) {
      if (error?.code === "EPERM") {
        t.skip("local TCP listen is not permitted in this environment");
        return;
      }
      throw error;
    }
    const dispatcherBaseUrl = `http://127.0.0.1:${dispatcherPort}`;

    const runnerAListening = await listenOrSkip(t, runnerA);
    if (!runnerAListening) {
      return;
    }
    const runnerBListening = await listenOrSkip(t, runnerB);
    if (!runnerBListening) {
      return;
    }
    runners.push(runnerAListening);
    runners.push(runnerBListening);

    dispatcher = spawn(
      "cargo",
      ["run", "--quiet", "-p", "wasmsh-dispatcher"],
      {
        cwd: process.cwd(),
        env: {
          ...process.env,
          PORT: String(dispatcherPort),
          HOST: "127.0.0.1",
          RUNNER_SERVICE_URLS: runners
            .map(({ port }) => `http://127.0.0.1:${port}`)
            .join(","),
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );

    let stderr = "";
    dispatcher.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });

    await waitForReady(dispatcherBaseUrl);

    const create = await jsonRequest(dispatcherBaseUrl, "/sessions", {
      method: "POST",
      body: {
        session_id: "affinity-session",
        initial_files: [
          {
            path: "/workspace/input.txt",
            content_base64: Buffer.from("dispatcher").toString("base64"),
          },
        ],
      },
    });
    assert.equal(create.response.status, 201, stderr);
    const firstWorkerId = create.payload.session.workerId;

    const init = await jsonRequest(dispatcherBaseUrl, "/sessions/affinity-session/init", {
      method: "POST",
      body: {},
    });
    assert.equal(init.response.status, 200, stderr);
    assert.equal(init.payload.result.workerId, firstWorkerId);

    const read = await jsonRequest(dispatcherBaseUrl, "/sessions/affinity-session/read-file", {
      method: "POST",
      body: { path: "/workspace/input.txt" },
    });
    assert.equal(read.response.status, 200, stderr);
    assert.equal(
      Buffer.from(read.payload.result.contentBase64, "base64").toString("utf8"),
      "dispatcher",
    );

    const second = await jsonRequest(dispatcherBaseUrl, "/sessions", {
      method: "POST",
      body: {
        session_id: "second-session",
      },
    });
    assert.equal(second.response.status, 201, stderr);
    assert.notEqual(second.payload.session.workerId, firstWorkerId);

    const close = await jsonRequest(dispatcherBaseUrl, "/sessions/affinity-session", {
      method: "DELETE",
    });
    assert.equal(close.response.status, 200, stderr);

    const missing = await jsonRequest(dispatcherBaseUrl, "/sessions/affinity-session/run", {
      method: "POST",
      body: { command: "echo no" },
    });
    assert.equal(missing.response.status, 404, stderr);
  } finally {
    if (dispatcher) {
      await stopProcess(dispatcher);
    }
    await runnerA.close();
    await runnerB.close();
  }
});
