import http from "node:http";

import { createRunner } from "./runner-main.mjs";
import { renderPrometheusMetrics } from "./metrics.mjs";

function json(response, statusCode, payload) {
  response.writeHead(statusCode, { "content-type": "application/json; charset=utf-8" });
  response.end(`${JSON.stringify(payload)}\n`);
}

export async function createRunnerServer(options = {}) {
  const runner = await createRunner(options);
  const port = options.port ?? Number(process.env.PORT ?? 8787);

  const server = http.createServer(async (request, response) => {
    if (!request.url) {
      json(response, 400, { ok: false, error: "missing url" });
      return;
    }

    if (request.url === "/healthz") {
      json(response, 200, { ok: true });
      return;
    }

    if (request.url === "/readyz") {
      const readiness = runner.readiness();
      json(response, readiness.ready ? 200 : 503, readiness);
      return;
    }

    if (request.url === "/metrics") {
      const body = renderPrometheusMetrics(runner.metrics.snapshot());
      response.writeHead(200, { "content-type": "text/plain; version=0.0.4; charset=utf-8" });
      response.end(body);
      return;
    }

    json(response, 404, { ok: false, error: "not found" });
  });

  return {
    runner,
    server,
    async listen() {
      await new Promise((resolveListen, rejectListen) => {
        server.once("error", rejectListen);
        server.listen(port, () => {
          server.off("error", rejectListen);
          resolveListen();
        });
      });
      return { port };
    },
    async close() {
      await new Promise((resolveClose, rejectClose) => {
        server.close((error) => {
          if (error) {
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
}
