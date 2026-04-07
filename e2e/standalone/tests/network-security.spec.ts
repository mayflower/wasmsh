/**
 * Network allowlist security tests — standalone browser (wasm-bindgen).
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
 */
import { test, expect } from "@playwright/test";

const FIXTURE_HOST = "localhost:3100";
const FIXTURE_URL = `http://${FIXTURE_HOST}/cors-echo.html`;

function decodeBytes(arr: number[]): string {
  return new TextDecoder().decode(new Uint8Array(arr));
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

async function initAndRun(
  page: any,
  allowedHosts: string[],
  command: string,
): Promise<{ stdout: string; stderr: string; exitCode: number | null }> {
  return page.evaluate(
    async ({
      hosts,
      cmd,
    }: {
      hosts: string[];
      cmd: string;
    }) => {
      const worker = (window as any).createShellWorker();

      function send(msg: any): Promise<any[]> {
        return new Promise((resolve, reject) => {
          const timeout = setTimeout(
            () => reject(new Error("worker timeout")),
            30_000,
          );
          worker.onmessage = (e: MessageEvent) => {
            clearTimeout(timeout);
            if (e.data.error) reject(new Error(e.data.error));
            else resolve(e.data.events);
          };
          worker.onerror = (e: ErrorEvent) => {
            clearTimeout(timeout);
            reject(new Error(e.message));
          };
          worker.postMessage(msg);
        });
      }

      await send({ type: "Init", step_budget: 0, allowed_hosts: hosts });
      const events = await send({ type: "Run", input: cmd });
      worker.terminate();

      // Extract results in browser context
      const stdoutParts: number[] = [];
      const stderrParts: number[] = [];
      let exitCode: number | null = null;
      for (const e of events) {
        if (e && typeof e === "object") {
          if ("Stdout" in e) stdoutParts.push(...e.Stdout);
          if ("Stderr" in e) stderrParts.push(...e.Stderr);
          if ("Exit" in e) exitCode = e.Exit;
        }
      }
      return {
        stdout: new TextDecoder().decode(new Uint8Array(stdoutParts)),
        stderr: new TextDecoder().decode(new Uint8Array(stderrParts)),
        exitCode,
      };
    },
    { hosts: allowedHosts, cmd: command },
  );
}

// ── Allowed host ────────────────────────────────────────────────

test("curl to allowed host (local fixture) succeeds", async ({ page }) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    [FIXTURE_HOST],
    `curl -sL ${FIXTURE_URL}`,
  );
  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
  expect(r.stdout).toMatch(/<(!|html|HTML)/);
});

test("wget to allowed host (local fixture) succeeds", async ({ page }) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    [FIXTURE_HOST],
    `wget -qO - ${FIXTURE_URL}`,
  );
  expect(r.exitCode).toBe(0);
  expect(r.stdout.length).toBeGreaterThan(0);
});

// ── Denied host ─────────────────────────────────────────────────
//
// The denied-host tests use fake external hostnames (example.com etc.).
// No actual fetch is attempted because the allowlist check fails first,
// so the tests do not depend on any external network being reachable.

test("curl to denied host (example.com) is blocked", async ({ page }) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    [FIXTURE_HOST],
    "curl https://example.com",
  );
  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toContain("denied");
});

test("wget to denied host (example.com) is blocked", async ({ page }) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    [FIXTURE_HOST],
    "wget -qO - https://example.com",
  );
  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toContain("denied");
});

// ── Subdomain not allowed with exact match ──────────────────────

test("curl to subdomain is blocked with exact host match", async ({
  page,
}) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    ["mayflower.de"],
    "curl https://evil.mayflower.de",
  );
  expect(r.exitCode).not.toBe(0);
});

// ── Similar hostnames blocked ───────────────────────────────────

test("curl to similar-looking hostnames is blocked", async ({ page }) => {
  await page.goto("/");
  for (const host of [
    "https://notmayflower.de",
    "https://mayflower.de.evil.com",
    "https://mayflower.com",
  ]) {
    const r = await initAndRun(page, ["mayflower.de"], `curl ${host}`);
    expect(r.exitCode).not.toBe(0);
  }
});

// ── Wildcard pattern ────────────────────────────────────────────

test("wildcard pattern allows base domain and blocks others", async ({
  page,
}) => {
  await page.goto("/");
  // `*.localhost:3100` should match the bare host `localhost:3100`
  // (HostAllowlist treats `*.X` as "X or any subdomain of X").
  const r = await initAndRun(
    page,
    [`*.${FIXTURE_HOST}`],
    `curl -sL ${FIXTURE_URL}`,
  );
  expect(r.exitCode).toBe(0);

  // But example.com is still blocked
  const r2 = await initAndRun(
    page,
    [`*.${FIXTURE_HOST}`],
    "curl https://example.com",
  );
  expect(r2.exitCode).not.toBe(0);
});

// ── Empty allowlist ─────────────────────────────────────────────

test("empty allowlist blocks all hosts", async ({ page }) => {
  await page.goto("/");
  // An empty allowlist creates a backend that denies every host.
  // See `WasmShell::init` in crates/wasmsh-browser/src/lib.rs.
  const r = await initAndRun(page, [], `curl ${FIXTURE_URL}`);
  expect(r.exitCode).not.toBe(0);
  expect(r.stderr).toMatch(/denied|allowlist/);
});

// ── curl | wc pipeline ──────────────────────────────────────────

test("curl piped to wc works with allowed host", async ({ page }) => {
  await page.goto("/");
  const r = await initAndRun(
    page,
    [FIXTURE_HOST],
    `curl -sL ${FIXTURE_URL} | wc -l`,
  );
  expect(r.exitCode).toBe(0);
  const lines = parseInt(r.stdout.trim(), 10);
  expect(lines).toBeGreaterThan(5);
});
