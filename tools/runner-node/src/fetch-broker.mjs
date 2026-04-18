import { assertAllowedHost, HostDeniedError } from "./network-policy.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
export const DEFAULT_BROKER_REQUEST_BYTES = 64 * 1024;
export const DEFAULT_BROKER_RESPONSE_BYTES = 1024 * 1024;
// Keep strictly below the 30 s guest-side Atomics.wait in createBrokerClient,
// otherwise the host keeps writing a response into shared buffers after the
// guest has already timed out and reset the control word.
const DEFAULT_FETCH_TIMEOUT_MS = 25_000;

export function createBrokerBuffers({
  requestBytes = DEFAULT_BROKER_REQUEST_BYTES,
  responseBytes = DEFAULT_BROKER_RESPONSE_BYTES,
} = {}) {
  return {
    controlBuffer: new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT * 3),
    requestBuffer: new SharedArrayBuffer(requestBytes),
    responseBuffer: new SharedArrayBuffer(responseBytes),
  };
}

async function readBodyWithLimit(response, byteLimit) {
  const declared = Number(response.headers.get("content-length") ?? NaN);
  if (Number.isFinite(declared) && declared > byteLimit) {
    throw new BrokerPayloadTooLargeError(
      `upstream Content-Length ${declared} exceeds broker body cap ${byteLimit}`,
    );
  }
  const reader = response.body?.getReader();
  if (!reader) {
    // No stream body (e.g. HEAD response); fall back to arrayBuffer which
    // we have already bounded via Content-Length above.
    const buffer = new Uint8Array(await response.arrayBuffer());
    if (buffer.byteLength > byteLimit) {
      throw new BrokerPayloadTooLargeError(
        `upstream body ${buffer.byteLength} exceeds broker body cap ${byteLimit}`,
      );
    }
    return buffer;
  }
  const chunks = [];
  let total = 0;
  while (true) {
    const { value, done } = await reader.read();
    if (done) {
      break;
    }
    total += value.byteLength;
    if (total > byteLimit) {
      await reader.cancel();
      throw new BrokerPayloadTooLargeError(
        `upstream body exceeded broker body cap ${byteLimit}`,
      );
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return out;
}

class BrokerPayloadTooLargeError extends Error {
  constructor(message) {
    super(message);
    this.name = "BrokerPayloadTooLargeError";
  }
}

function classifyBrokerError(error) {
  if (error instanceof HostDeniedError) {
    return "host_denied";
  }
  if (error instanceof BrokerPayloadTooLargeError) {
    return "payload_too_large";
  }
  if (error?.name === "AbortError" || error?.name === "TimeoutError") {
    return "timeout";
  }
  return "transport";
}

export function createFetchBroker({
  fetchImpl = fetch,
  metrics = null,
  fetchTimeoutMs = DEFAULT_FETCH_TIMEOUT_MS,
  responseByteLimit = DEFAULT_BROKER_RESPONSE_BYTES,
} = {}) {
  return {
    async fetchJson(request, allowedHosts) {
      assertAllowedHost(request.url, allowedHosts);
      const response = await fetchImpl(request.url, {
        method: request.method,
        headers: request.headers,
        body: request.body_base64 ? Buffer.from(request.body_base64, "base64") : undefined,
        redirect: request.follow_redirects ? "follow" : "manual",
        signal: AbortSignal.timeout(fetchTimeoutMs),
      });
      // Reserve ~1 KiB of the response envelope for headers + JSON framing
      // so a body that fits under the cap still serialises into the shared
      // buffer without overflow.
      const bodyCap = Math.max(0, responseByteLimit - 1024);
      const bodyBytes = await readBodyWithLimit(response, bodyCap);
      return {
        status: response.status,
        headers: Array.from(response.headers.entries()),
        body_base64: Buffer.from(bodyBytes).toString("base64"),
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
        const reason = classifyBrokerError(error);
        if (reason === "host_denied") {
          metrics?.hostDenied?.();
        } else {
          metrics?.brokerFetchError?.(reason);
        }
        response = {
          status: 0,
          headers: [],
          body_base64: "",
          error: error instanceof Error ? error.message : String(error),
          error_reason: reason,
        };
      }

      const responseJson = encoder.encode(JSON.stringify(response));
      if (responseJson.length > responseView.byteLength) {
        metrics?.brokerFetchError?.("response_overflow");
        const encoded = encoder.encode(JSON.stringify({
          status: 0,
          headers: [],
          body_base64: "",
          error: "broker response exceeds configured buffer size",
          error_reason: "response_overflow",
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
          error_reason: "request_overflow",
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
          error_reason: "timeout",
        };
      }
      const responseLength = Atomics.load(control, 2);
      const responseJson = decoder.decode(responseView.subarray(0, responseLength));
      Atomics.store(control, 0, 0);
      return JSON.parse(responseJson);
    },
  };
}
