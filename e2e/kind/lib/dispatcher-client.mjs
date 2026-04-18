// Minimal HTTP client for the dispatcher's control-plane API.
// Tests only need a handful of endpoints, so this is hand-rolled rather than
// pulling in a fetch-wrapper dependency.
export function createDispatcherClient({ baseUrl, defaultTimeoutMs = 30_000 }) {
  async function request(method, path, { body, timeoutMs } = {}) {
    const signal = AbortSignal.timeout(timeoutMs ?? defaultTimeoutMs);
    const init = { method, signal, headers: {} };
    if (body !== undefined) {
      init.headers["content-type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const response = await fetch(new URL(path, baseUrl), init);
    const text = await response.text();
    let parsed;
    if (text.length > 0) {
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = { raw: text };
      }
    }
    return { status: response.status, ok: response.ok, body: parsed };
  }

  return {
    healthz: () => request("GET", "/healthz"),
    readyz: () => request("GET", "/readyz"),
    createSession: (payload) => request("POST", "/sessions", { body: payload }),
    runInSession: (sessionId, command) =>
      request("POST", `/sessions/${sessionId}/run`, { body: { command } }),
    writeFile: (sessionId, path, contentBase64) =>
      request("POST", `/sessions/${sessionId}/write-file`, {
        body: { path, contentBase64 },
      }),
    readFile: (sessionId, path) =>
      request("POST", `/sessions/${sessionId}/read-file`, { body: { path } }),
    listDir: (sessionId, path) =>
      request("POST", `/sessions/${sessionId}/list-dir`, { body: { path } }),
    closeSession: (sessionId) => request("DELETE", `/sessions/${sessionId}`),
  };
}
