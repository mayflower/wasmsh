/**
 * Shared protocol helpers for node-host and browser-worker.
 *
 * Centralises event parsing and base64 encoding so that both host
 * implementations stay in sync.
 */

const decoder = new TextDecoder();

/**
 * Collect all byte arrays from events matching `key` (e.g. "Stdout").
 */
export function extractStream(events, key) {
  const chunks = [];
  let total = 0;
  for (const event of events) {
    if (event && typeof event === "object" && key in event) {
      const chunk = event[key];
      chunks.push(chunk);
      total += chunk.length;
    }
  }
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.length;
  }
  return result;
}

/**
 * Return the exit code from an event array, or null if none.
 */
export function getExitCode(events) {
  for (const event of events) {
    if (event && typeof event === "object" && "Exit" in event) {
      return event.Exit;
    }
  }
  return null;
}

/**
 * Return the Version string from an event array, or null.
 */
export function getVersion(events) {
  for (const event of events) {
    if (event && typeof event === "object" && "Version" in event) {
      return event.Version;
    }
  }
  return null;
}

/**
 * Decode base64 string to Uint8Array (works in both Node and browser).
 */
export function decodeBase64(text) {
  if (typeof Buffer !== "undefined") {
    return new Uint8Array(Buffer.from(text, "base64"));
  }
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Encode Uint8Array to base64 string (works in both Node and browser).
 */
export function encodeBase64(bytes) {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(bytes).toString("base64");
  }
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

/**
 * Build a standard run() response from a raw event array.
 */
export function buildRunResult(events) {
  const stdout = decoder.decode(extractStream(events, "Stdout"));
  const stderr = decoder.decode(extractStream(events, "Stderr"));
  return {
    events,
    stdout,
    stderr,
    output: stdout + stderr,
    exitCode: getExitCode(events),
  };
}
