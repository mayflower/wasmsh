// Unit tests for the JS allowlist parity with the Rust HostAllowlist.
//
// The fetch membrane and any JS-side policy checks call into
// `isHostAllowed()`. Audit F9 flagged that the JS side accepted
// non-http(s) URLs (ws://, ftp://, file://, javascript:, ...) that
// Rust's HostAllowlist::check explicitly rejects. These tests pin the
// scheme allowlist and the trailing-dot normalization that pulls the
// two implementations into sync.
import { test } from "node:test";
import assert from "node:assert/strict";

import { isHostAllowed } from "../../../packages/npm/wasmsh-pyodide/lib/allowlist.mjs";

test("isHostAllowed rejects every non-http(s) scheme", () => {
  const allow = ["example.com"];
  for (const url of [
    "file:///etc/passwd",
    "data:text/plain,abc",
    "javascript:alert(1)",
    "ws://example.com",
    "wss://example.com",
    "ftp://example.com",
    "blob:example.com",
  ]) {
    assert.equal(isHostAllowed(url, allow), false, `must reject ${url}`);
  }
});

test("isHostAllowed normalises the FQDN trailing dot", () => {
  assert.equal(isHostAllowed("https://example.com./path", ["example.com"]), true);
  assert.equal(isHostAllowed("https://api.example.com./", ["*.example.com"]), true);
});

test("isHostAllowed wildcard does not match the apex (parity with Rust)", () => {
  assert.equal(isHostAllowed("https://example.com/", ["*.example.com"]), false);
  assert.equal(isHostAllowed("https://api.example.com/", ["*.example.com"]), true);
});

test("isHostAllowed honors per-pattern port (non-default)", () => {
  // URL parsers omit the port when it matches the scheme's default (443
  // for https, 80 for http) so the per-port test uses a non-default
  // port — same constraint applies in the Rust HostAllowlist.
  assert.equal(isHostAllowed("https://api.example.com:8443/", ["api.example.com:8443"]), true);
  assert.equal(isHostAllowed("https://api.example.com:8080/", ["api.example.com:8443"]), false);
});
