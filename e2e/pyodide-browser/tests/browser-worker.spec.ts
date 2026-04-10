import { test, expect } from "@playwright/test";

/**
 * Sends a command to the Pyodide worker and returns the events array.
 */
async function send(page: any, msg: any): Promise<any[]> {
  return page.evaluate(async (m: any) => {
    return (window as any)._pySend(m);
  }, msg);
}

function decodeBytes(arr: number[]): string {
  return new TextDecoder().decode(new Uint8Array(arr));
}

// ── Setup: navigate + wait for worker ready ─────────────────

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => (window as any)._pyWorkerReadyPromise);
});

// ── Tests ───────────────────────────────────────────────────

test("echo browser returns stdout and exit 0", async ({ page }) => {
  const events = await send(page, { type: "Run", input: "echo browser" });
  const stdout = events.find((e: any) => "Stdout" in e);
  expect(stdout).toBeTruthy();
  expect(decodeBytes(stdout.Stdout)).toBe("browser\n");
  const exit = events.find((e: any) => "Exit" in e);
  expect(exit).toBeTruthy();
  expect(exit.Exit).toBe(0);
});

test("python3 -c print('py') returns stdout py", async ({ page }) => {
  const events = await send(page, {
    type: "Run",
    input: "python3 -c \"print('py')\"",
  });
  const stdout = events.find((e: any) => "Stdout" in e);
  expect(stdout).toBeTruthy();
  expect(decodeBytes(stdout.Stdout)).toBe("py\n");
  const exit = events.find((e: any) => "Exit" in e);
  expect(exit.Exit).toBe(0);
});

test("shell writes file, Python reads it", async ({ page }) => {
  // Shell writes
  await send(page, {
    type: "Run",
    input: "echo shell-content > /workspace/from_sh.txt",
  });

  // Python reads
  const events = await send(page, {
    type: "Run",
    input: `python3 -c "print(open('/workspace/from_sh.txt').read(), end='')"`,
  });
  const stdout = events.find((e: any) => "Stdout" in e);
  expect(stdout).toBeTruthy();
  expect(decodeBytes(stdout.Stdout)).toBe("shell-content\n");
});

test("Python writes file, shell reads it", async ({ page }) => {
  // Python writes
  await send(page, {
    type: "Run",
    input: `python3 -c "open('/workspace/from_py.txt','w').write('py-content')"`,
  });

  // Shell reads
  const events = await send(page, {
    type: "Run",
    input: "cat /workspace/from_py.txt",
  });
  const stdout = events.find((e: any) => "Stdout" in e);
  expect(stdout).toBeTruthy();
  expect(decodeBytes(stdout.Stdout)).toBe("py-content");
  const exit = events.find((e: any) => "Exit" in e);
  expect(exit.Exit).toBe(0);
});
