/**
 * Network allowlist security tests — Pyodide browser (Emscripten).
 *
 * Verifies that curl/wget can only reach hosts in the allowed_hosts list.
 * Uses mayflower.de as the test target.
 *
 * Tests run through the Pyodide browser worker which uses Emscripten's
 * extern "C" FFI for network calls (wasmsh_js_http_fetch → sync XHR).
 */
import { test, expect } from "@playwright/test";

function decodeBytes(arr: number[]): string {
  return new TextDecoder().decode(new Uint8Array(arr));
}

async function send(page: any, msg: any): Promise<any[]> {
  return page.evaluate(
    async (m: any) => {
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("worker timeout")),
          60_000,
        );
        const worker = (window as any)._pyWorker;
        worker.onmessage = (e: MessageEvent) => {
          clearTimeout(timeout);
          if (e.data.error) reject(new Error(e.data.error));
          else resolve(e.data.events);
        };
        worker.postMessage(m);
      });
    },
    msg,
  );
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
  await page.waitForFunction(
    () => (window as any)._pyWorkerReady === true,
    { timeout: 90_000 },
  );
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

test("curl to allowed host (mayflower.de) succeeds", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
  const r = await run(page, "curl -sL https://mayflower.de");

  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
  expect(r.stdout).toMatch(/<(!|html|HTML)/);
});

test("wget to allowed host (mayflower.de) succeeds", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
  const r = await run(page, "wget -qO - https://mayflower.de");

  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
});

// ── Denied host ─────────────────────────────────────────────────

test("curl to denied host (example.com) is blocked", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
  const r = await run(page, "curl https://example.com");

  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toContain("denied");
});

test("wget to denied host (example.com) is blocked", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
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

test("wildcard *.mayflower.de allows base domain but blocks others", async ({
  page,
}) => {
  await initWithHosts(page, ["*.mayflower.de"]);
  const r = await run(page, "curl -sL https://mayflower.de");
  expect(r.exitCode).toBe(0);

  const r2 = await run(page, "curl https://example.com");
  expect(r2.exitCode).not.toBe(0);
});

// ── Empty allowlist ─────────────────────────────────────────────

test("empty allowlist blocks all hosts", async ({ page }) => {
  await initWithHosts(page, []);
  const r = await run(page, "curl https://mayflower.de");

  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toMatch(/denied|allowlist/);
});

// ── curl piped to wc ────────────────────────────────────────────

test("curl piped to wc works with allowed host", async ({ page }) => {
  await initWithHosts(page, ["mayflower.de"]);
  const r = await run(page, "curl -sL https://mayflower.de | wc -l");

  expect(r.exitCode).toBe(0);
  const lines = parseInt(r.stdout.trim(), 10);
  expect(lines).toBeGreaterThan(5);
});
