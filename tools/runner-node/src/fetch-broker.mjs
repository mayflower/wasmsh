import { assertAllowedHost } from "./network-policy.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export function createBrokerBuffers({
  requestBytes = 64 * 1024,
  responseBytes = 4 * 1024 * 1024,
} = {}) {
  return {
    controlBuffer: new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT * 3),
    requestBuffer: new SharedArrayBuffer(requestBytes),
    responseBuffer: new SharedArrayBuffer(responseBytes),
  };
}

export function createFetchBroker({
  fetchImpl = fetch,
  metrics = null,
} = {}) {
  return {
    async fetchJson(request, allowedHosts) {
      assertAllowedHost(request.url, allowedHosts);
      const response = await fetchImpl(request.url, {
        method: request.method,
        headers: request.headers,
        body: request.body_base64 ? Buffer.from(request.body_base64, "base64") : undefined,
        redirect: request.follow_redirects ? "follow" : "manual",
      });
      const body = Buffer.from(await response.arrayBuffer()).toString("base64");
      return {
        status: response.status,
        headers: Array.from(response.headers.entries()),
        body_base64: body,
      };
    },
    async handleFetchMessage(message) {
      const control = new Int32Array(message.controlBuffer);
      const requestView = new Uint8Array(message.requestBuffer);
      const responseView = new Uint8Array(message.responseBuffer);
      const requestLength = Atomics.load(control, 1);
      const requestJson = decoder.decode(requestView.subarray(0, requestLength));
      const request = JSON.parse(requestJson);

      let response;
      try {
        response = await this.fetchJson(request, message.allowedHosts);
      } catch (error) {
        if (String(error.message ?? error).includes("host denied")) {
          metrics?.hostDenied();
        }
        response = {
          status: 0,
          headers: [],
          body_base64: "",
          error: error instanceof Error ? error.message : String(error),
        };
      }

      const responseJson = encoder.encode(JSON.stringify(response));
      if (responseJson.length > responseView.byteLength) {
        const encoded = encoder.encode(JSON.stringify({
          status: 0,
          headers: [],
          body_base64: "",
          error: "broker response exceeds configured buffer size",
        }));
        responseView.set(encoded, 0);
        Atomics.store(control, 2, encoded.length);
      } else {
        responseView.set(responseJson, 0);
        Atomics.store(control, 2, responseJson.length);
      }
      Atomics.store(control, 0, 2);
      Atomics.notify(control, 0);
    },
  };
}

export function createBrokerClient({
  parentPort,
  controlBuffer,
  requestBuffer,
  responseBuffer,
}) {
  const control = new Int32Array(controlBuffer);
  const requestView = new Uint8Array(requestBuffer);
  const responseView = new Uint8Array(responseBuffer);

  return {
    fetchSync(url, method, headersJson, bodyBase64, followRedirects) {
      const payload = encoder.encode(JSON.stringify({
        url,
        method,
        headers: JSON.parse(headersJson || "[]"),
        body_base64: bodyBase64 || "",
        follow_redirects: Boolean(followRedirects),
      }));
      if (payload.length > requestView.byteLength) {
        return {
          status: 0,
          headers: [],
          body_base64: "",
          error: "broker request exceeds configured buffer size",
        };
      }

      requestView.set(payload, 0);
      Atomics.store(control, 1, payload.length);
      Atomics.store(control, 0, 1);
      parentPort.postMessage({ type: "broker-fetch" });
      const result = Atomics.wait(control, 0, 1, 30_000);
      if (result === "timed-out") {
        return {
          status: 0,
          headers: [],
          body_base64: "",
          error: "broker fetch timed out",
        };
      }
      const responseLength = Atomics.load(control, 2);
      const responseJson = decoder.decode(responseView.subarray(0, responseLength));
      Atomics.store(control, 0, 0);
      return JSON.parse(responseJson);
    },
  };
}
