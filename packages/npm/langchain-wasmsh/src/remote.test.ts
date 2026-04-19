import { describe, expect, it, vi } from "vitest";

import { WasmshRemoteSandbox } from "./remote.js";

const BASE_URL = "http://dispatcher.test";
const SESSION_ID = "test-session";

interface FetchCall {
  url: string;
  init: RequestInit;
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function makeFetch(
  routes: Array<{
    match: (call: FetchCall) => boolean;
    respond: (call: FetchCall) => Response | Promise<Response>;
  }>,
): { fetch: typeof globalThis.fetch; calls: FetchCall[] } {
  const calls: FetchCall[] = [];
  const fetchImpl = vi.fn(
    async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      const call: FetchCall = { url, init };
      calls.push(call);
      for (const route of routes) {
        if (route.match(call)) {
          return route.respond(call);
        }
      }
      throw new Error(`Unexpected fetch to ${init.method ?? "GET"} ${url}`);
    },
  ) as unknown as typeof globalThis.fetch;
  return { fetch: fetchImpl, calls };
}

function bodyOf(call: FetchCall): Record<string, unknown> {
  return JSON.parse(call.init.body as string);
}

function createSessionRoute(sessionId = SESSION_ID) {
  return {
    match: (call: FetchCall) =>
      call.url === `${BASE_URL}/sessions` && call.init.method === "POST",
    respond: () =>
      jsonResponse(201, {
        ok: true,
        session: { sessionId, workerId: "worker-1" },
      }),
  };
}

describe("WasmshRemoteSandbox.create", () => {
  it("posts session creation with payload", async () => {
    const { fetch, calls } = makeFetch([createSessionRoute()]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      allowedHosts: ["pypi.org"],
      stepBudget: 42,
      initialFiles: { "/workspace/a.txt": "hi" },
      fetch,
    });
    expect(sandbox.id).toBe(SESSION_ID);
    expect(calls).toHaveLength(1);
    const body = bodyOf(calls[0]);
    expect(body.session_id).toBe(SESSION_ID);
    expect(body.allowed_hosts).toEqual(["pypi.org"]);
    expect(body.step_budget).toBe(42);
    expect(body.initial_files).toEqual([
      { path: "/workspace/a.txt", contentBase64: "aGk=" },
    ]);
  });

  it("prefers server-assigned session id", async () => {
    const { fetch } = makeFetch([createSessionRoute("server-chosen")]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      fetch,
    });
    expect(sandbox.id).toBe("server-chosen");
  });

  it("strips trailing slashes from dispatcher url", async () => {
    const { fetch, calls } = makeFetch([createSessionRoute()]);
    await WasmshRemoteSandbox.create({
      dispatcherUrl: `${BASE_URL}/`,
      sessionId: SESSION_ID,
      fetch,
    });
    expect(calls[0].url).toBe(`${BASE_URL}/sessions`);
  });

  it("throws with dispatcher error on 5xx", async () => {
    const { fetch } = makeFetch([
      {
        match: (call) => call.url === `${BASE_URL}/sessions`,
        respond: () =>
          jsonResponse(503, { ok: false, error: "no runner available" }),
      },
    ]);
    await expect(
      WasmshRemoteSandbox.create({ dispatcherUrl: BASE_URL, fetch }),
    ).rejects.toThrow(/no runner available/);
  });

  it("forwards custom headers on every request", async () => {
    const { fetch, calls } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/run"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "", exitCode: 0 },
          }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      headers: { Authorization: "Bearer t" },
      fetch,
    });
    await sandbox.execute("echo hi");
    for (const call of calls) {
      const headers = call.init.headers as Record<string, string>;
      expect(headers["Authorization"] ?? headers["authorization"]).toBe(
        "Bearer t",
      );
    }
  });
});

describe("execute", () => {
  it("prepends cd to the working directory", async () => {
    const { fetch, calls } = makeFetch([
      createSessionRoute(),
      {
        match: (call) =>
          call.url === `${BASE_URL}/sessions/${SESSION_ID}/run`,
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "hello\n", exitCode: 0 },
          }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    const result = await sandbox.execute("echo hello");
    expect(result.output).toBe("hello\n");
    expect(result.exitCode).toBe(0);
    const runCall = calls[calls.length - 1];
    const body = bodyOf(runCall);
    expect(body.command).toBe("cd '/workspace' && echo hello");
  });

  it("surfaces non-zero exit codes", async () => {
    const { fetch } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/run"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "oops\n", exitCode: 127 },
          }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    const result = await sandbox.execute("nope");
    expect(result.exitCode).toBe(127);
  });
});

describe("uploadFiles / downloadFiles", () => {
  it("round-trips base64 content", async () => {
    const { fetch, calls } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/write-file"),
        respond: () =>
          jsonResponse(200, { ok: true, result: { events: [] } }),
      },
      {
        match: (call) => call.url.endsWith("/run"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "\n", exitCode: 0 },
          }),
      },
      {
        match: (call) => call.url.endsWith("/read-file"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: {
              contentBase64: Buffer.from("hello", "utf8").toString("base64"),
              events: [],
            },
          }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });

    const payload = new TextEncoder().encode("hello");
    const upload = await sandbox.uploadFiles([
      ["/workspace/x.txt", payload],
    ]);
    expect(upload[0].error).toBeNull();
    const writeCall = calls.find((c) => c.url.endsWith("/write-file"))!;
    const writeBody = bodyOf(writeCall);
    expect(Buffer.from(writeBody.contentBase64 as string, "base64")).toEqual(
      Buffer.from(payload),
    );

    const download = await sandbox.downloadFiles(["/workspace/x.txt"]);
    expect(download[0].error).toBeNull();
    expect(new TextDecoder().decode(download[0].content!)).toBe("hello");
  });

  it("rejects relative upload paths without calling dispatcher", async () => {
    const { fetch, calls } = makeFetch([createSessionRoute()]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    const result = await sandbox.uploadFiles([["relative.txt", new Uint8Array()]]);
    expect(result[0].error).toBe("invalid_path");
    expect(calls.every((c) => !c.url.endsWith("/write-file"))).toBe(true);
  });

  it("maps directory precheck to is_directory without reading", async () => {
    const { fetch, calls } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/run"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "DIR\n", exitCode: 0 },
          }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    const result = await sandbox.downloadFiles(["/workspace"]);
    expect(result[0].error).toBe("is_directory");
    expect(calls.every((c) => !c.url.endsWith("/read-file"))).toBe(true);
  });

  it("maps 404 to file_not_found", async () => {
    const { fetch } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/run"),
        respond: () =>
          jsonResponse(200, {
            ok: true,
            result: { output: "\n", exitCode: 0 },
          }),
      },
      {
        match: (call) => call.url.endsWith("/read-file"),
        respond: () =>
          jsonResponse(404, { ok: false, error: "file not found: /missing" }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    const [resp] = await sandbox.downloadFiles(["/missing"]);
    expect(resp.error).toBe("file_not_found");
  });
});

describe("close / stop", () => {
  it("sends close and delete on stop", async () => {
    const { fetch, calls } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith(`/sessions/${SESSION_ID}/close`),
        respond: () => jsonResponse(200, { ok: true, result: {} }),
      },
      {
        match: (call) =>
          call.init.method === "DELETE" &&
          call.url.endsWith(`/sessions/${SESSION_ID}`),
        respond: () => jsonResponse(200, { ok: true, result: {} }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    await sandbox.stop();
    expect(calls.some((c) => c.url.endsWith(`/${SESSION_ID}/close`))).toBe(
      true,
    );
    expect(
      calls.some(
        (c) =>
          c.init.method === "DELETE" && c.url.endsWith(`/sessions/${SESSION_ID}`),
      ),
    ).toBe(true);
    expect(sandbox.isRunning).toBe(false);
  });

  it("is idempotent", async () => {
    let closeCalls = 0;
    const { fetch } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/close"),
        respond: () => {
          closeCalls++;
          return jsonResponse(200, { ok: true, result: {} });
        },
      },
      {
        match: (call) => call.init.method === "DELETE",
        respond: () => jsonResponse(200, { ok: true, result: {} }),
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    await sandbox.close();
    await sandbox.close();
    expect(closeCalls).toBe(1);
  });

  it("swallows dispatcher errors during teardown", async () => {
    const { fetch } = makeFetch([
      createSessionRoute(),
      {
        match: (call) => call.url.endsWith("/close"),
        respond: () =>
          jsonResponse(503, { ok: false, error: "dispatcher down" }),
      },
      {
        match: (call) => call.init.method === "DELETE",
        respond: () => {
          throw new Error("network refused");
        },
      },
    ]);
    const sandbox = await WasmshRemoteSandbox.create({
      dispatcherUrl: BASE_URL,
      sessionId: SESSION_ID,
      fetch,
    });
    // Must not throw.
    await sandbox.close();
  });
});
