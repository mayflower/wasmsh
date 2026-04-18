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

async function readJson(request) {
  const chunks = [];
  for await (const chunk of request) {
    chunks.push(typeof chunk === "string" ? Buffer.from(chunk) : chunk);
  }
  if (chunks.length === 0) {
    return {};
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
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
        const body = await readJson(request);
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
