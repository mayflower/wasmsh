/**
 * Browser Playwright tests for micropip-based package installation.
 *
 * Uses a committed wheel fixture (e2e/fixtures/wasmsh_test_fixture-*.whl)
 * uploaded to the sandbox EmscriptenFs and installed via `emfs:` URL.
 */
import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { test, expect } from "@playwright/test";

const __dirname = dirname(fileURLToPath(import.meta.url));
const FIXTURES_DIR = resolve(__dirname, "../../fixtures");
const WHEEL_PATH = resolve(
  FIXTURES_DIR,
  "wasmsh_test_fixture-0.1.0-py3-none-any.whl",
);

/**
 * Send a JSON-RPC request to the npm package worker and return the result.
 */
async function send(page: any, method: string, params: any = {}): Promise<any> {
  return page.evaluate(
    async ([m, p]: [string, any]) => {
      return (window as any)._npmSend(m, p);
    },
    [method, params],
  );
}

function toBase64(data: Uint8Array | Buffer): string {
  return Buffer.from(data).toString("base64");
}

// ── Setup: navigate + wait for worker ready ─────────────────

test.beforeEach(async ({ page }) => {
  await page.goto("/npm-test.html");
  await page.waitForFunction(
    () => (window as any)._npmWorkerReady === true,
    { timeout: 90_000 },
  );
});

// ── Tests ───────────────────────────────────────────────────

test("session exposes installPythonPackages method", async ({ page }) => {
  // Check the method exists on the worker protocol
  const hasMethod = await page.evaluate(async () => {
    // The npm-test.html page exposes _npmSend; check if installPythonPackages
    // is a recognized method by the worker.
    try {
      await (window as any)._npmSend("installPythonPackages", {
        requirements: [],
      });
      return true;
    } catch (e: any) {
      // "unknown method" means it doesn't exist yet — that's expected
      return !e.message.includes("unknown method");
    }
  });
  expect(hasMethod).toBe(true);
});

test("installs a local wheel from emfs: and imports it", async ({ page }) => {
  // Upload wheel fixture to sandbox FS
  const wheelBytes = readFileSync(WHEEL_PATH);
  await send(page, "writeFile", {
    path: "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
    contentBase64: toBase64(wheelBytes),
  });

  // Install from emfs: URL
  const result = await send(page, "installPythonPackages", {
    requirements: "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
  });
  expect(result).toBeTruthy();

  // Verify the package is importable
  const run = await send(page, "run", {
    command:
      'python3 -c "from wasmsh_test_fixture import GREETING; print(GREETING)"',
  });
  expect(run.stdout.trim()).toBe("hello from wasmsh_test_fixture");
  expect(run.exitCode).toBe(0);
});

test("rejects file: URIs for security", async ({ page }) => {
  const threw = await page.evaluate(async () => {
    try {
      await (window as any)._npmSend("installPythonPackages", {
        requirements: "file:///etc/passwd",
      });
      return false;
    } catch {
      return true;
    }
  });
  expect(threw).toBe(true);
});
