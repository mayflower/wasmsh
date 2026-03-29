/**
 * Browser Playwright tests for the wasmsh-pyodide npm package's browser-worker.js.
 *
 * These test the package-level browser worker API (JSON-RPC over postMessage)
 * rather than the raw Pyodide worker protocol.
 *
 * Requires built Pyodide assets symlinked at fixture/dist/.
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

function toBase64(text: string): string {
  return Buffer.from(text).toString("base64");
}

// ── Setup: navigate to npm-test.html + wait for worker ready ──

test.beforeEach(async ({ page }) => {
  await page.goto("/npm-test.html");
  await page.waitForFunction(
    () => (window as any)._npmWorkerReady === true,
    { timeout: 90_000 },
  );
});

// ── Tests ─────────────────────────────────────────────────────

test("echo returns stdout and exitCode 0", async ({ page }) => {
  const result = await send(page, "run", { command: "echo hello" });
  expect(result.stdout.trim()).toBe("hello");
  expect(result.exitCode).toBe(0);
});

test("python3 -c produces correct output", async ({ page }) => {
  const result = await send(page, "run", {
    command: 'python3 -c "print(42)"',
  });
  expect(result.stdout.trim()).toBe("42");
  expect(result.exitCode).toBe(0);
});

test("shared FS: bash writes, python reads", async ({ page }) => {
  await send(page, "run", {
    command: "echo 'from-bash' > /workspace/shared.txt",
  });

  const result = await send(page, "run", {
    command: `python3 -c "print(open('/workspace/shared.txt').read().strip())"`,
  });
  expect(result.stdout.trim()).toBe("from-bash");
});

test("writeFile + readFile roundtrip", async ({ page }) => {
  const contentBase64 = toBase64("roundtrip-data");
  await send(page, "writeFile", {
    path: "/workspace/rt.txt",
    contentBase64,
  });

  const result = await send(page, "readFile", {
    path: "/workspace/rt.txt",
  });

  // Decode the base64 response and verify
  const decoded = Buffer.from(result.contentBase64, "base64").toString("utf-8");
  expect(decoded).toBe("roundtrip-data");
});

test("listDir shows written files", async ({ page }) => {
  await send(page, "writeFile", {
    path: "/workspace/listed.txt",
    contentBase64: toBase64("content"),
  });

  const result = await send(page, "listDir", { path: "/workspace" });
  expect(result.output).toContain("listed.txt");
});
