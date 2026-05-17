import http from "node:http";
import { URL } from "node:url";

import { createRunner } from "./runner-main.mjs";
import { renderPrometheusMetrics } from "./metrics.mjs";

function json(response, statusCode, payload) {
  response.writeHead(statusCode, { "content-type": "application/json; charset=utf-8" });
  response.end(`${JSON.stringify(payload)}\n`);
}

function decodeContentBase64(contentBase64) {
  return Uint8Array.from(Buffer.from(contentBase64 ?? "", "base64"));
}

// Default request-body ceiling for the runner control plane (32 MiB).
// Matches the dispatcher's MAX_REQUEST_BODY_BYTES so the dispatcher
// can't forward a payload that the runner will then reject. The cap
// applies even to authenticated callers — an authenticated-but-buggy
// dispatcher must not be able to OOM the runner with a single request.
const DEFAULT_MAX_REQUEST_BODY_BYTES = 32 * 1024 * 1024;

class RequestBodyTooLargeError extends Error {
  constructor(bytes, limit) {
    super(`request body exceeds limit (${bytes} > ${limit} bytes)`);
    this.code = "WASMSH_REQUEST_BODY_TOO_LARGE";
    this.bytes = bytes;
    this.limit = limit;
  }
}

async function readJson(request, { maxBytes = DEFAULT_MAX_REQUEST_BODY_BYTES } = {}) {
  const chunks = [];
  let received = 0;
  for await (const chunk of request) {
    const buf = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
    received += buf.byteLength;
    if (received > maxBytes) {
      // Drain remaining stream to let the socket close cleanly, then
      // throw. Without the cap the runner buffers the entire body before
      // any handler runs, so an unauthenticated client (or a buggy
      // dispatcher) can drive memory pressure with a single POST.
      throw new RequestBodyTooLargeError(received, maxBytes);
    }
    chunks.push(buf);
  }
  if (chunks.length === 0) {
    return {};
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

// Constant-time-ish bearer compare. The runner token is short and not a
// user secret, but matching the dispatcher's posture keeps consistent
// behavior across the two HTTP surfaces. Length check is unavoidable;
// the byte loop accumulates so an early-out doesn't leak timing.
function bearerEquals(supplied, expected) {
  if (supplied.length !== expected.length) return false;
  let diff = 0;
  for (let i = 0; i < expected.length; i += 1) {
    diff |= supplied.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return diff === 0;
}

function extractBearer(request) {
  const raw = request.headers["authorization"];
  if (typeof raw !== "string") return "";
  if (raw.startsWith("Bearer ")) return raw.slice("Bearer ".length);
  if (raw.startsWith("bearer ")) return raw.slice("bearer ".length);
  return "";
}

function methodNotAllowed(response) {
  json(response, 405, { ok: false, error: "method not allowed" });
}

function sessionNotFound(response, sessionId) {
  json(response, 404, {
    ok: false,
    error: `session not found: ${sessionId}`,
  });
}

export async function createRunnerServer(options = {}) {
  const runner = await createRunner(options);
  const port = options.port ?? Number(process.env.PORT ?? 8787);
  const host = options.host ?? process.env.HOST ?? "0.0.0.0";
  // Optional bearer token. When set, every endpoint EXCEPT /healthz and
  // /readyz requires `Authorization: Bearer <token>`. The Helm chart wires
  // this from a shared Secret with the dispatcher so cross-pod traffic
  // gets the same posture. When unset, the runner is open (legacy
  // behavior); production deployments rely on a NetworkPolicy that
  // restricts ingress to the dispatcher.
  const authToken = options.authToken
    ?? process.env.WASMSH_RUNNER_AUTH_TOKEN
    ?? "";
  let maxRequestBodyBytes = options.maxRequestBodyBytes;
  if (typeof maxRequestBodyBytes !== "number" || maxRequestBodyBytes <= 0) {
    const fromEnv = Number(process.env.WASMSH_RUNNER_MAX_REQUEST_BYTES || 0);
    maxRequestBodyBytes = fromEnv > 0 ? fromEnv : DEFAULT_MAX_REQUEST_BODY_BYTES;
  }

  const server = http.createServer(async (request, response) => {
    try {
      if (!request.url) {
        json(response, 400, { ok: false, error: "missing url" });
        return;
      }

      const requestUrl = new URL(request.url, "http://runner.local");
      const path = requestUrl.pathname;

      if (path === "/healthz") {
        json(response, 200, { ok: true });
        return;
      }

      if (path === "/readyz") {
        const readiness = runner.readiness();
        json(response, readiness.ready ? 200 : 503, readiness);
        return;
      }

      // Everything past this point is privileged. Gate on bearer token
      // when configured. /healthz and /readyz stay open above so
      // liveness/readiness probes work without a credential.
      if (authToken) {
        if (!bearerEquals(extractBearer(request), authToken)) {
          json(response, 401, { ok: false, error: "unauthorized" });
          return;
        }
      }

      if (path === "/metrics") {
        const body = renderPrometheusMetrics(runner.metrics.snapshot());
        response.writeHead(200, { "content-type": "text/plain; version=0.0.4; charset=utf-8" });
        response.end(body);
        return;
      }

      if (path === "/runner/snapshot") {
        if (request.method !== "GET") {
          methodNotAllowed(response);
          return;
        }
        json(response, 200, {
          ok: true,
          runner: runner.runnerSnapshot(),
        });
        return;
      }

      if (path === "/runner/drain") {
        if (request.method !== "POST") {
          methodNotAllowed(response);
          return;
        }
        const result = runner.drain();
        json(response, 200, {
          ok: true,
          ...result,
        });
        return;
      }

      if (path === "/sessions") {
        if (request.method === "GET") {
          json(response, 200, {
            ok: true,
            sessions: runner.listSessions(),
          });
          return;
        }
        if (request.method !== "POST") {
          methodNotAllowed(response);
          return;
        }
        const body = await readJson(request, { maxBytes: maxRequestBodyBytes });
        const session = await runner.createSession({
          sessionId: body.sessionId,
          allowedHosts: body.allowedHosts ?? [],
          stepBudget: body.stepBudget ?? 0,
          initialFiles: (body.initialFiles ?? []).map((file) => ({
            path: file.path,
            content: decodeContentBase64(file.contentBase64),
          })),
        });
        const init = await session.init();
        json(response, 201, {
          ok: true,
          session: {
            sessionId: session.id,
            workerId: session.workerId,
            restoreMetrics: session.restoreMetrics,
            init,
          },
        });
        return;
      }

      const match = path.match(/^\/sessions\/([^/]+)(?:\/([^/]+))?$/);
      if (!match) {
        json(response, 404, { ok: false, error: "not found" });
        return;
      }

      const [, sessionId, action] = match;
      const session = runner.getSession(sessionId);
      if (!session) {
        sessionNotFound(response, sessionId);
        return;
      }

      if (!action) {
        if (request.method !== "DELETE") {
          methodNotAllowed(response);
          return;
        }
        const result = await session.close();
        json(response, 200, {
          ok: true,
          sessionId,
          result,
        });
        return;
      }

      if (request.method !== "POST") {
        methodNotAllowed(response);
        return;
      }

      const body = await readJson(request);
      switch (action) {
        case "init": {
          const result = await session.init();
          json(response, 200, {
            ok: true,
            sessionId,
            result,
          });
          return;
        }
        case "run": {
          const result = await session.run(body.command ?? "");
          json(response, 200, {
            ok: true,
            sessionId,
            result,
          });
          return;
        }
        case "write-file": {
          const result = await session.writeFile(
            body.path,
            decodeContentBase64(body.contentBase64),
          );
          json(response, 200, {
            ok: true,
            sessionId,
            result,
          });
          return;
        }
        case "read-file": {
          const result = await session.readFile(body.path);
          json(response, 200, {
            ok: true,
            sessionId,
            result: {
              ...result,
              contentBase64: Buffer.from(result.content).toString("base64"),
            },
          });
          return;
        }
        case "list-dir": {
          const result = await session.listDir(body.path);
          json(response, 200, {
            ok: true,
            sessionId,
            result,
          });
          return;
        }
        case "close": {
          const result = await session.close();
          json(response, 200, {
            ok: true,
            sessionId,
            result,
          });
          return;
        }
        default:
          json(response, 404, { ok: false, error: "not found" });
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (error?.code === "E_RUNNER_DRAINING") {
        json(response, 503, { ok: false, error: message, code: error.code });
        return;
      }
      if (error?.code === "WASMSH_SESSION_EXISTS") {
        json(response, 409, { ok: false, error: message, code: error.code });
        return;
      }
      if (error?.code === "WASMSH_REQUEST_BODY_TOO_LARGE") {
        json(response, 413, { ok: false, error: message, code: error.code });
        return;
      }
      // Internal runner service — log 500s server-side so operators can
      // correlate them.  Everything leaving this process is already
      // observed via Prometheus; this console.error is the fallback for
      // uncategorised bugs.
      console.error("runner 500:", message);
      json(response, 500, { ok: false, error: message });
    }
  });

  return {
    runner,
    server,
    async listen() {
      await new Promise((resolveListen, rejectListen) => {
        server.once("error", rejectListen);
        server.listen(port, host, () => {
          server.off("error", rejectListen);
          resolveListen();
        });
      });
      return { port: server.address().port };
    },
    async close() {
      await new Promise((resolveClose, rejectClose) => {
        server.close((error) => {
          if (error) {
            if (error.code === "ERR_SERVER_NOT_RUNNING") {
              resolveClose();
              return;
            }
            rejectClose(error);
            return;
          }
          resolveClose();
        });
      });
      await runner.close();
    },
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const service = await createRunnerServer();
  await service.listen();

  const shutdown = async (signal) => {
    // Kubernetes rolling update: on SIGTERM we flip drain so the
    // dispatcher stops assigning new sessions, wait briefly for
    // in-flight work, then close the HTTP listener and runner.
    console.error(`received ${signal}; draining runner`);
    try {
      service.runner.drain();
    } catch (error) {
      console.error("runner drain failed:", error);
    }
    try {
      await service.close();
    } catch (error) {
      console.error("runner shutdown failed:", error);
      process.exitCode = 1;
    }
    process.exit();
  };
  process.once("SIGTERM", () => { shutdown("SIGTERM"); });
  process.once("SIGINT", () => { shutdown("SIGINT"); });
}
