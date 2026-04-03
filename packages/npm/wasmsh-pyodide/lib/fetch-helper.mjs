/**
 * Standalone fetch helper for synchronous HTTP from the WASM runtime.
 *
 * Reads a JSON request from stdin, performs the fetch, writes the JSON
 * response to stdout. Used by node-module.mjs via spawnSync to provide
 * synchronous HTTP to Emscripten.
 *
 * Input:  {"url", "method", "headers", "body_base64", "follow_redirects"}
 * Output: {"status", "headers", "body_base64", "error?"}
 */

let input = "";
for await (const chunk of process.stdin) {
  input += chunk;
}

try {
  const req = JSON.parse(input);
  const opts = {
    method: req.method,
    headers: Object.fromEntries(req.headers || []),
    redirect: req.follow_redirects ? "follow" : "manual",
  };
  if (req.body_base64) {
    opts.body = Buffer.from(req.body_base64, "base64");
  }

  const resp = await fetch(req.url, opts);
  const body = Buffer.from(await resp.arrayBuffer());
  const headers = [...resp.headers.entries()];

  process.stdout.write(
    JSON.stringify({
      status: resp.status,
      headers,
      body_base64: body.toString("base64"),
    }),
  );
} catch (e) {
  process.stdout.write(
    JSON.stringify({ status: 0, headers: [], body_base64: "", error: e.message }),
  );
}
