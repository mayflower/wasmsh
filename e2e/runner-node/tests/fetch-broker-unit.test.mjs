import { test } from "node:test";
import assert from "node:assert/strict";

import {
  createBrokerBuffers,
  createBrokerClient,
  createFetchBroker,
  DEFAULT_BROKER_REQUEST_BYTES,
  DEFAULT_BROKER_RESPONSE_BYTES,
} from "../../../tools/runner-node/src/fetch-broker.mjs";
import { HostDeniedError } from "../../../tools/runner-node/src/network-policy.mjs";

function decodeResponse(buffers) {
  const control = new Int32Array(buffers.controlBuffer);
  const responseView = new Uint8Array(buffers.responseBuffer);
  const length = Atomics.load(control, 2);
  return JSON.parse(new TextDecoder().decode(responseView.subarray(0, length)));
}

function writeRequestIntoBuffers(buffers, request) {
  const control = new Int32Array(buffers.controlBuffer);
  const requestView = new Uint8Array(buffers.requestBuffer);
  const payload = new TextEncoder().encode(JSON.stringify(request));
  requestView.set(payload, 0);
  Atomics.store(control, 1, payload.length);
  Atomics.store(control, 0, 1);
}

function trackingMetrics() {
  const state = { hostDenied: 0, fetchErrors: new Map() };
  return {
    state,
    hostDenied() {
      state.hostDenied += 1;
    },
    brokerFetchError(reason) {
      state.fetchErrors.set(reason, (state.fetchErrors.get(reason) ?? 0) + 1);
    },
  };
}

test("createBrokerBuffers honours size overrides", () => {
  const buffers = createBrokerBuffers({ requestBytes: 2048, responseBytes: 4096 });
  assert.equal(buffers.requestBuffer.byteLength, 2048);
  assert.equal(buffers.responseBuffer.byteLength, 4096);
});

test("createBrokerBuffers uses documented defaults when no options are passed", () => {
  const buffers = createBrokerBuffers();
  assert.equal(buffers.requestBuffer.byteLength, DEFAULT_BROKER_REQUEST_BYTES);
  assert.equal(buffers.responseBuffer.byteLength, DEFAULT_BROKER_RESPONSE_BYTES);
});

test("handleFetchMessage classifies host_denied with a counter, not a substring match", async () => {
  const metrics = trackingMetrics();
  const broker = createFetchBroker({
    fetchImpl: async () => {
      throw new Error("unreachable: assertAllowedHost must reject first");
    },
    metrics,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 4096 });
  writeRequestIntoBuffers(buffers, {
    url: "https://example.com",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["mayflower.de"] });
  const response = decodeResponse(buffers);
  assert.equal(response.status, 0);
  assert.equal(response.error_reason, "host_denied");
  assert.equal(metrics.state.hostDenied, 1);
  assert.equal(metrics.state.fetchErrors.size, 0);
});

test("handleFetchMessage records a transport error when the fetch impl throws unrelated", async () => {
  const metrics = trackingMetrics();
  const broker = createFetchBroker({
    fetchImpl: async () => {
      throw new TypeError("connection reset by peer");
    },
    metrics,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 4096 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.status, 0);
  assert.equal(response.error_reason, "transport");
  assert.equal(metrics.state.hostDenied, 0);
  assert.equal(metrics.state.fetchErrors.get("transport"), 1);
});

test("handleFetchMessage surfaces timeout reason when fetch is aborted", async () => {
  const metrics = trackingMetrics();
  const broker = createFetchBroker({
    fetchImpl: async (_url, init) => {
      // Simulate the signal being aborted mid-fetch.
      if (init?.signal) {
        const error = new Error("aborted");
        error.name = "AbortError";
        throw error;
      }
      return new Response("ok");
    },
    metrics,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 4096 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.error_reason, "timeout");
  assert.equal(metrics.state.fetchErrors.get("timeout"), 1);
});

test("handleFetchMessage rejects responses exceeding the body cap via Content-Length", async () => {
  const metrics = trackingMetrics();
  const broker = createFetchBroker({
    fetchImpl: async () => {
      return new Response(new Uint8Array(256), {
        status: 200,
        headers: { "content-length": "9999999" },
      });
    },
    metrics,
    responseByteLimit: 8192,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 8192 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.error_reason, "payload_too_large");
  assert.equal(metrics.state.fetchErrors.get("payload_too_large"), 1);
});

test("handleFetchMessage streams and caps bodies when Content-Length is missing", async () => {
  const metrics = trackingMetrics();
  // A 10 KiB payload with no Content-Length header; the broker must
  // abort the reader once the cap is exceeded instead of buffering
  // the whole body.
  const broker = createFetchBroker({
    fetchImpl: async () => {
      const stream = new ReadableStream({
        start(controller) {
          controller.enqueue(new Uint8Array(4096));
          controller.enqueue(new Uint8Array(4096));
          controller.enqueue(new Uint8Array(4096));
          controller.close();
        },
      });
      return new Response(stream, {
        status: 200,
        headers: { "content-type": "application/octet-stream" },
      });
    },
    metrics,
    responseByteLimit: 4096,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 8192 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.error_reason, "payload_too_large");
});

test("handleFetchMessage succeeds for a happy-path in-range response", async () => {
  const metrics = trackingMetrics();
  const broker = createFetchBroker({
    fetchImpl: async () => {
      return new Response("payload", { status: 200 });
    },
    metrics,
  });
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 4096 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.status, 200);
  assert.equal(Buffer.from(response.body_base64, "base64").toString("utf8"), "payload");
  assert.equal(metrics.state.fetchErrors.size, 0);
});

test("handleFetchMessage returns response_overflow when envelope is larger than shared buffer", async () => {
  const metrics = trackingMetrics();
  const body = "x".repeat(2048);
  const broker = createFetchBroker({
    fetchImpl: async () => new Response(body, { status: 200 }),
    metrics,
    responseByteLimit: 8192,
  });
  // Tiny response buffer forces the overflow branch.
  const buffers = createBrokerBuffers({ requestBytes: 1024, responseBytes: 256 });
  writeRequestIntoBuffers(buffers, {
    url: "https://allowed.test",
    method: "GET",
    headers: [],
    body_base64: "",
    follow_redirects: false,
  });
  await broker.handleFetchMessage({ ...buffers, allowedHosts: ["allowed.test"] });
  const response = decodeResponse(buffers);
  assert.equal(response.error_reason, "response_overflow");
  assert.equal(metrics.state.fetchErrors.get("response_overflow"), 1);
});

test("assertAllowedHost throws HostDeniedError for off-list hosts", async () => {
  const broker = createFetchBroker({
    fetchImpl: async () => new Response("ok"),
  });
  await assert.rejects(
    () => broker.fetchJson({
      url: "https://off-list.test",
      method: "GET",
      headers: [],
      body_base64: "",
      follow_redirects: false,
    }, ["allowed.test"]),
    (error) => {
      assert.ok(error instanceof HostDeniedError);
      assert.equal(error.url, "https://off-list.test");
      return true;
    },
  );
});

test("createBrokerClient returns request_overflow when payload exceeds request buffer", () => {
  const buffers = createBrokerBuffers({ requestBytes: 64, responseBytes: 1024 });
  const fakeParent = { postMessage: () => {} };
  const client = createBrokerClient({
    parentPort: fakeParent,
    controlBuffer: buffers.controlBuffer,
    requestBuffer: buffers.requestBuffer,
    responseBuffer: buffers.responseBuffer,
  });
  const result = client.fetchSync(
    "https://example.com/with/a/long/path",
    "GET",
    "[]",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    false,
  );
  assert.equal(result.status, 0);
  assert.equal(result.error_reason, "request_overflow");
});
