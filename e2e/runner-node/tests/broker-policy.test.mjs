import { test } from "node:test";
import assert from "node:assert/strict";

import { createFetchBroker } from "../../../tools/runner-node/src/fetch-broker.mjs";

test("broker enforces allowed_hosts before dispatching fetch", async () => {
  const broker = createFetchBroker({
    fetchImpl: async () => new Response("ok", { status: 200 }),
  });

  const allowed = await broker.fetchJson({
    url: "https://mayflower.de",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: true,
  }, ["mayflower.de"]);
  assert.equal(allowed.status, 200);

  await assert.rejects(
    () => broker.fetchJson({
      url: "https://example.com",
      method: "GET",
      headers: [],
      body_base64: "",
      follow_redirects: true,
    }, ["mayflower.de"]),
    /host denied/,
  );

  const wildcard = await broker.fetchJson({
    url: "https://sub.mayflower.de",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: true,
  }, ["*.mayflower.de"]);
  assert.equal(wildcard.status, 200);
});
