/**
 * Standalone fetch helper for synchronous HTTP from the WASM runtime.
 *
 * Reads a JSON request from stdin, performs the fetch, writes the JSON
 * response to stdout. Used by node-module.mjs via spawnSync to provide
 * synchronous HTTP to Emscripten.
 *
 * Input:  {"url", "method", "headers", "body_base64", "follow_redirects",
 *          "options": {"timeout_ms", "max_response_bytes", "max_redirs", ...}}
 * Output: {"status", "headers", "body_base64", "error?"}
 *
 * Security caps (B1 + part of B4):
 * - `timeout_ms` enforced via AbortController
 * - `max_response_bytes` enforced first by Content-Length precheck, then by
 *   streaming the body and aborting once the cap is exceeded. Without this,
 *   `await resp.arrayBuffer()` would buffer the entire response in memory
 *   before any utility-layer check.
 */

const DEFAULT_TIMEOUT_MS = 30000;
const DEFAULT_MAX_RESPONSE_BYTES = 64 * 1024 * 1024;

let input = "";
for await (const chunk of process.stdin) {
  input += chunk;
}

function writeError(message) {
  process.stdout.write(
    JSON.stringify({
      status: 0,
      headers: [],
      body_base64: "",
      error: message,
    }),
  );
}

try {
  const req = JSON.parse(input);
  const opts = req.options || {};
  const timeoutMs =
    typeof opts.timeout_ms === "number" && opts.timeout_ms > 0
      ? opts.timeout_ms
      : DEFAULT_TIMEOUT_MS;
  const maxResponseBytes =
    typeof opts.max_response_bytes === "number" && opts.max_response_bytes > 0
      ? opts.max_response_bytes
      : DEFAULT_MAX_RESPONSE_BYTES;

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(new Error("timeout")), timeoutMs);

  const fetchOpts = {
    method: req.method,
    headers: Object.fromEntries(req.headers || []),
    redirect: req.follow_redirects ? "follow" : "manual",
    signal: controller.signal,
  };
  if (req.body_base64) {
    fetchOpts.body = Buffer.from(req.body_base64, "base64");
  }

  let resp;
  try {
    resp = await fetch(req.url, fetchOpts);
  } finally {
    // Don't clear yet — still need the timeout active while we read the body.
  }

  // Content-Length precheck: refuse before draining the socket.
  const contentLength = Number(resp.headers.get("content-length"));
  if (Number.isFinite(contentLength) && contentLength > maxResponseBytes) {
    clearTimeout(timer);
    try { controller.abort(); } catch { /* already aborted */ }
    writeError(
      `response exceeds max_response_bytes (Content-Length ${contentLength} > ${maxResponseBytes})`,
    );
  } else {
    // Stream and cap. If the server lies about Content-Length or omits it,
    // this is what actually enforces the limit.
    const chunks = [];
    let received = 0;
    let truncated = false;
    if (resp.body) {
      const reader = resp.body.getReader();
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        if (value) {
          received += value.byteLength;
          if (received > maxResponseBytes) {
            truncated = true;
            try { await reader.cancel(); } catch { /* swallow */ }
            try { controller.abort(); } catch { /* swallow */ }
            break;
          }
          chunks.push(value);
        }
      }
    }

    clearTimeout(timer);

    if (truncated) {
      writeError(
        `response exceeds max_response_bytes (streamed ${received} > ${maxResponseBytes})`,
      );
    } else {
      const body = Buffer.concat(chunks.map((c) => Buffer.from(c)));
      const headers = [...resp.headers.entries()];
      process.stdout.write(
        JSON.stringify({
          status: resp.status,
          headers,
          body_base64: body.toString("base64"),
        }),
      );
    }
  }
} catch (e) {
  writeError(e?.message || String(e));
}
