/**
 * Browser Playwright tests for micropip-based package installation.
 *
 * Uses a committed wheel fixture (e2e/fixtures/wasmsh_test_fixture-*.whl)
 * uploaded to the sandbox EmscriptenFs and installed via `emfs:` URL.
 */
import { test, expect } from "@playwright/test";

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

// Read the wheel fixture at import time via Playwright's Node.js context.
// We avoid top-level `import * as fs from "node:fs"` because Playwright's
// TS transpiler mishandles it in this project setup.
const wheelBase64 = (() => {
  // eslint-disable-next-line @typescript-eslint/no-require-imports
  const fs = require("node:fs");
  const path = require("node:path");
  const wheelPath = path.resolve(
    __dirname, "../../fixtures/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
  );
  return fs.readFileSync(wheelPath).toString("base64");
})();

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
  const hasMethod = await page.evaluate(async () => {
    try {
      await (window as any)._npmSend("installPythonPackages", {
        requirements: [],
      });
      return true;
    } catch (e: any) {
      return !e.message.includes("unknown method");
    }
  });
  expect(hasMethod).toBe(true);
});

test("installs a local wheel from emfs: and imports it", async ({ page }) => {
  await send(page, "writeFile", {
    path: "/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
    contentBase64: wheelBase64,
  });

  const result = await send(page, "installPythonPackages", {
    requirements: "emfs:/tmp/wasmsh_test_fixture-0.1.0-py3-none-any.whl",
  });
  expect(result).toBeTruthy();

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
