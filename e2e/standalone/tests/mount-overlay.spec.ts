import { test, expect } from "@playwright/test";

/**
 * E2E coverage for the lazy copy-on-write overlay exposed via WasmShell.mount
 * (ADR-0033). A read-only base is mounted at the root; reads fall through to it,
 * writes copy-on-write into the upper layer, deletes whiteout the base entry,
 * and re-init clears the overlay.
 */

async function initWorker(page: any) {
  return page.evaluate(async () => {
    const worker = (window as any).createShellWorker();
    (window as any)._testWorker = worker;

    function sendAndReceive(msg: any): Promise<any> {
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(
          () => reject(new Error("worker timeout")),
          10_000,
        );
        worker.onmessage = (e: MessageEvent) => {
          clearTimeout(timeout);
          resolve(e.data);
        };
        worker.onerror = (e: ErrorEvent) => {
          clearTimeout(timeout);
          reject(new Error(e.message));
        };
        worker.postMessage(msg);
      });
    }

    (window as any)._send = sendAndReceive;

    return sendAndReceive({ type: "Init", step_budget: 0 });
  });
}

function send(page: any, msg: any): Promise<any> {
  return page.evaluate(async (m: any) => {
    const send = (window as any)._send;
    return send(m);
  }, msg);
}

function stdoutOf(reply: any): string {
  const evt = reply.events.find((e: any) => "Stdout" in e);
  return evt ? new TextDecoder().decode(new Uint8Array(evt.Stdout)) : "";
}

function fsChangedPaths(reply: any): string[] {
  return reply.events
    .filter((e: any) => "FsChanged" in e)
    .map((e: any) => e.FsChanged);
}

test("mounted base file reads through the overlay", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  await send(page, {
    type: "Mount",
    base: { "/base/readme.txt": "hello from base" },
  });

  const run = await send(page, { type: "Run", input: "cat /base/readme.txt" });
  expect(stdoutOf(run)).toBe("hello from base");
  // A pure read of a base file does not surface as a change.
  expect(fsChangedPaths(run)).toEqual([]);
});

test("writing a base file is copy-on-write and emits FsChanged", async ({
  page,
}) => {
  await page.goto("/");
  await initWorker(page);

  await send(page, {
    type: "Mount",
    base: { "/base/readme.txt": "original" },
  });

  const write = await send(page, {
    type: "Run",
    input: "echo changed > /base/readme.txt",
  });
  expect(write.events.find((e: any) => "Exit" in e).Exit).toBe(0);
  expect(fsChangedPaths(write)).toEqual(["/base/readme.txt"]);

  const read = await send(page, { type: "Run", input: "cat /base/readme.txt" });
  expect(stdoutOf(read)).toBe("changed\n");
});

test("deleting a base file whiteouts it from listings", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  await send(page, {
    type: "Mount",
    base: { "/proj/a.txt": "a", "/proj/b.txt": "b" },
  });

  const rm = await send(page, { type: "Run", input: "rm /proj/a.txt" });
  expect(rm.events.find((e: any) => "Exit" in e).Exit).toBe(0);

  const list = await send(page, { type: "ListDir", path: "/proj" });
  const listing = stdoutOf(list);
  expect(listing).not.toContain("a.txt");
  expect(listing).toContain("b.txt");
});

test("re-init clears the mounted overlay", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  await send(page, {
    type: "Mount",
    base: { "/seed.txt": "data" },
  });
  // Confirm it is visible before re-init.
  const before = await send(page, { type: "Run", input: "cat /seed.txt" });
  expect(stdoutOf(before)).toBe("data");

  // Re-initialize: the overlay (and its base) is rebuilt empty.
  await send(page, { type: "Init", step_budget: 0 });
  const after = await send(page, { type: "Run", input: "cat /seed.txt" });
  expect(after.events.find((e: any) => "Exit" in e).Exit).not.toBe(0);
});
