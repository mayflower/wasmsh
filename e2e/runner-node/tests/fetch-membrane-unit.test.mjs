// Unit tests for the Pyodide JS fetch membrane.
//
// These exercise the broker behavior in isolation against a minimal
// canned-fetch double — no Pyodide, no real network. They cover the
// audit-F3 requirements:
//   - per-hop allowlist re-validation on redirects
//   - Content-Length precheck
//   - streaming body cap
//   - method/body downgrade on 301/302/303
//   - too-many-redirects bound
//   - HostDeniedError on the initial URL
//
// The membrane mutates `globalThis.fetch`, so each test runs inside a
// helper that snapshots and restores `globalThis.fetch` to keep the test
// file's own fetch behavior isolated.
import { test } from "node:test";
import assert from "node:assert/strict";

import { installFetchMembrane } from "../../../packages/npm/wasmsh-pyodide/lib/fetch-membrane.mjs";

function withMembrane(allowedHosts, fakeFetch, fn) {
  const savedFetch = globalThis.fetch;
  globalThis.fetch = fakeFetch;
  // Force a fresh membrane install: drop the existing flag if any.
  const flag = Symbol.for("wasmsh.fetch-membrane");
  if (Object.getOwnPropertyDescriptor(globalThis, flag)) {
    delete globalThis[flag];
  }
  installFetchMembrane(globalThis, allowedHosts);
  return Promise.resolve(fn(globalThis.fetch)).finally(() => {
    if (Object.getOwnPropertyDescriptor(globalThis, flag)) {
      delete globalThis[flag];
    }
    globalThis.fetch = savedFetch;
  });
}

test("initial host denied by allowlist throws before any fetch", async () => {
  let called = 0;
  const fake = async () => {
    called += 1;
    return new Response("", { status: 200 });
  };
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    await assert.rejects(
      () => brokered("https://denied.test/x"),
      (e) => e.code === "WASMSH_HOST_DENIED",
    );
    assert.equal(called, 0, "transport must not be reached for denied initial URL");
  });
});

test("302 to disallowed host is rejected with no second fetch", async () => {
  const calls = [];
  const fake = async (input, init) => {
    const url = typeof input === "string" ? input : input.url;
    calls.push(url);
    // Force callers to remember they asked for manual redirect handling.
    assert.equal(init.redirect, "manual");
    if (calls.length === 1) {
      return new Response("", {
        status: 302,
        headers: { Location: "https://attacker.test/loot" },
      });
    }
    return new Response("LEAK", { status: 200 });
  };
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    await assert.rejects(
      () => brokered("https://allowed.test/redir"),
      (e) => e.code === "WASMSH_HOST_DENIED",
    );
    assert.deepEqual(calls, ["https://allowed.test/redir"]);
  });
});

test("302 to allowed subdomain is followed (per-hop re-check passes)", async () => {
  const calls = [];
  const fake = async (input, init) => {
    const url = typeof input === "string" ? input : input.url;
    calls.push(url);
    assert.equal(init.redirect, "manual");
    if (calls.length === 1) {
      return new Response("", {
        status: 302,
        headers: { Location: "https://api.allowed.test/data" },
      });
    }
    return new Response("hello", { status: 200 });
  };
  await withMembrane(["allowed.test", "*.allowed.test"], fake, async (brokered) => {
    const res = await brokered("https://allowed.test/start");
    const body = await res.text();
    assert.equal(body, "hello");
    assert.deepEqual(calls, [
      "https://allowed.test/start",
      "https://api.allowed.test/data",
    ]);
  });
});

test("302 to loopback IP is rejected by per-hop check", async () => {
  const calls = [];
  const fake = async (input) => {
    const url = typeof input === "string" ? input : input.url;
    calls.push(url);
    if (calls.length === 1) {
      return new Response("", {
        status: 302,
        headers: { Location: "http://127.0.0.1:8080/private" },
      });
    }
    return new Response("LEAK", { status: 200 });
  };
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    await assert.rejects(
      () => brokered("https://allowed.test/redir"),
      (e) => e.code === "WASMSH_HOST_DENIED",
    );
    assert.deepEqual(calls, ["https://allowed.test/redir"]);
  });
});

test("Content-Length precheck rejects before draining", async () => {
  const cap = 1024;
  const big = "A".repeat(cap * 4);
  const fake = async () =>
    new Response(big, {
      status: 200,
      headers: { "content-length": String(big.length) },
    });
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    // Drop the cap on the membrane's state symbol so we can exercise it
    // without 64 MiB allocations.
    const flag = Symbol.for("wasmsh.fetch-membrane");
    globalThis[flag].maxResponseBytes = cap;
    await assert.rejects(
      () => brokered("https://allowed.test/big"),
      (e) => e.code === "WASMSH_RESPONSE_TOO_LARGE",
    );
  });
});

test("streaming cap rejects when Content-Length is absent or lies", async () => {
  const cap = 64;
  const fake = async () => {
    // Build a Response whose body emits more bytes than Content-Length claims.
    const stream = new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("A".repeat(cap * 2)));
        controller.close();
      },
    });
    return new Response(stream, { status: 200 });
  };
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    const flag = Symbol.for("wasmsh.fetch-membrane");
    globalThis[flag].maxResponseBytes = cap;
    const res = await brokered("https://allowed.test/stream");
    // The streaming cap fires only when the body is actually read.
    await assert.rejects(
      () => res.arrayBuffer(),
      (e) => e?.code === "WASMSH_RESPONSE_TOO_LARGE",
    );
  });
});

test("301 POST is downgraded to GET on redirect", async () => {
  const calls = [];
  const fake = async (input, init) => {
    const url = typeof input === "string" ? input : input.url;
    calls.push({ url, method: init.method, body: init.body });
    if (calls.length === 1) {
      return new Response("", {
        status: 301,
        headers: { Location: "https://api.allowed.test/v2" },
      });
    }
    return new Response("ok", { status: 200 });
  };
  await withMembrane(["allowed.test", "*.allowed.test"], fake, async (brokered) => {
    await brokered("https://allowed.test/legacy", { method: "POST", body: "payload" });
    assert.equal(calls[0].method, "POST");
    assert.equal(calls[0].body, "payload");
    assert.equal(calls[1].method, "GET");
    assert.equal(calls[1].body, undefined);
  });
});

test("redirect chain longer than the cap is rejected", async () => {
  let n = 0;
  const fake = async () => {
    n += 1;
    return new Response("", {
      status: 302,
      headers: { Location: `https://allowed.test/hop/${n}` },
    });
  };
  await withMembrane(["allowed.test"], fake, async (brokered) => {
    const flag = Symbol.for("wasmsh.fetch-membrane");
    globalThis[flag].maxRedirects = 3;
    await assert.rejects(
      () => brokered("https://allowed.test/start"),
      (e) => e.code === "WASMSH_TOO_MANY_REDIRECTS",
    );
  });
});
