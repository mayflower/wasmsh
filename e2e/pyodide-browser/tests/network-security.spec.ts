/**
 * Network allowlist security tests — Pyodide browser (Emscripten).
 *
 * Verifies that curl/wget can only reach hosts in the allowed_hosts list.
 *
 * The "allowed host" tests fetch a static fixture page served by the
 * Playwright dev server itself (`fixture/cors-echo.html`).  Talking to
 * a same-origin URL avoids the cross-origin problem with sync XHR
 * (no CORS preflight, no opaque-response body) and removes the test
 * dependency on any external network.  The "denied host" tests still
 * use a fake external hostname because the allowlist check happens
 * before any actual fetch is attempted.
 *
 * Tests run through the Pyodide browser worker which uses Emscripten's
 * extern "C" FFI for network calls (wasmsh_js_http_fetch → sync XHR).
 */
import { test, expect } from "@playwright/test";

const FIXTURE_HOST = "localhost:3200";
const FIXTURE_URL = `http://${FIXTURE_HOST}/cors-echo.html`;

function decodeBytes(arr: number[]): string {
  return new TextDecoder().decode(new Uint8Array(arr));
}

async function send(page: any, msg: any): Promise<any[]> {
  return page.evaluate(async (m: any) => (window as any)._pySend(m), msg);
}

function findStdout(events: any[]): string {
  const parts: number[] = [];
  for (const e of events) {
    if (e && typeof e === "object" && "Stdout" in e) parts.push(...e.Stdout);
  }
  return decodeBytes(parts);
}

function findStderr(events: any[]): string {
  const parts: number[] = [];
  for (const e of events) {
    if (e && typeof e === "object" && "Stderr" in e) parts.push(...e.Stderr);
  }
  return decodeBytes(parts);
}

function findExitCode(events: any[]): number | null {
  for (const e of events) {
    if (e && typeof e === "object" && "Exit" in e) return e.Exit;
  }
  return null;
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => (window as any)._pyWorkerReadyPromise);
});

async function initWithHosts(page: any, allowedHosts: string[]) {
  await send(page, {
    type: "Init",
    step_budget: 0,
    allowed_hosts: allowedHosts,
  });
}

async function run(page: any, command: string) {
  const events = await send(page, { type: "Run", input: command });
  return {
    stdout: findStdout(events),
    stderr: findStderr(events),
    exitCode: findExitCode(events),
  };
}

// ── Allowed host ────────────────────────────────────────────────

test("curl to allowed host (local fixture) succeeds", async ({ page }) => {
  await initWithHosts(page, [FIXTURE_HOST]);
  const r = await run(page, `curl -sL ${FIXTURE_URL}`);

  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
  expect(r.stdout).toMatch(/<(!|html|HTML)/);
});

test("wget to allowed host (local fixture) succeeds", async ({ page }) => {
  await initWithHosts(page, [FIXTURE_HOST]);
  const r = await run(page, `wget -qO - ${FIXTURE_URL}`);

  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
});

// ── Denied host ─────────────────────────────────────────────────
//
// The denied-host tests use fake external hostnames (example.com etc.).
// No actual fetch is attempted because the allowlist check fails first,
// so the tests do not depend on any external network being reachable.

test("curl to denied host (example.com) is blocked", async ({ page }) => {
  await initWithHosts(page, [FIXTURE_HOST]);
  const r = await run(page, "curl https://example.com");

  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toContain("denied");
});

test("wget to denied host (example.com) is blocked", async ({ page }) => {
  await initWithHosts(page, [FIXTURE_HOST]);
  const r = await run(page, "wget -qO - https://example.com");

  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toContain("denied");
});

// ── Subdomain blocked with exact match ──────────────────────────

test("curl to subdomain is blocked with exact host match", async ({
  page,
}) => {
  await initWithHosts(page, ["mayflower.de"]);
  const r = await run(page, "curl https://evil.mayflower.de");

  expect(r.exitCode).not.toBe(0);
});

// ── Similar hostnames blocked ───────────────────────────────────

test("curl to similar-looking hostnames is blocked", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
  for (const host of [
    "https://notmayflower.de",
    "https://mayflower.de.evil.com",
    "https://mayflower.com",
  ]) {
    const r = await run(page, `curl ${host}`);
    expect(r.exitCode).not.toBe(0);
  }
});

// ── Wildcard pattern ────────────────────────────────────────────

test("wildcard pattern allows base domain and blocks others", async ({
  page,
}) => {
  // `*.localhost:3200` should match the bare host `localhost:3200`
  // (HostAllowlist treats `*.X` as "X or any subdomain of X").
  await initWithHosts(page, [`*.${FIXTURE_HOST}`]);
  const r = await run(page, `curl -sL ${FIXTURE_URL}`);
  expect(r.exitCode).toBe(0);

  const r2 = await run(page, "curl https://example.com");
  expect(r2.exitCode).not.toBe(0);
});

// ── Empty allowlist ─────────────────────────────────────────────

test("empty allowlist blocks all hosts", async ({ page }) => {
  // An empty allowlist creates a backend that denies every host.
  // See `wasmsh_runtime_command` in crates/wasmsh-pyodide/src/lib.rs.
  await initWithHosts(page, []);
  const r = await run(page, `curl ${FIXTURE_URL}`);

  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toMatch(/denied|allowlist/);
});

// ── curl piped to wc ────────────────────────────────────────────

test("curl piped to wc works with allowed host", async ({ page }) => {
  await initWithHosts(page, [FIXTURE_HOST]);
  const r = await run(page, `curl -sL ${FIXTURE_URL} | wc -l`);

  expect(r.exitCode).toBe(0);
  const lines = parseInt(r.stdout.trim(), 10);
  expect(lines).toBeGreaterThan(5);
});
