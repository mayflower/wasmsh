import { test, expect } from "@playwright/test";

/**
 * Helper: create a worker, init it, and return a sendAndReceive function.
 */
async function initWorker(page: any) {
  return page.evaluate(async () => {
    const worker = (window as any).createShellWorker();

    // Expose a global so subsequent evaluate() calls can use it.
    (window as any)._testWorker = worker;
    (window as any)._seq = 0;

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

    // Init with unlimited budget.
    const initReply = await sendAndReceive({
      type: "Init",
      step_budget: 0,
    });
    return initReply;
  });
}

/**
 * Helper: send a command to the already-initialized worker.
 */
function send(page: any, msg: any): Promise<any> {
  return page.evaluate(async (m: any) => {
    const send = (window as any)._send;
    return send(m);
  }, msg);
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

test("WriteFile then ReadFile round-trips binary content", async ({
  page,
}) => {
  await page.goto("/");
  await initWorker(page);

  // WriteFile
  const writeReply = await send(page, {
    type: "WriteFile",
    path: "/workspace/a.txt",
    // "alpha" as UTF-8 byte array
    data: Array.from(new TextEncoder().encode("alpha")),
  });

  // Should contain FsChanged event
  const fsChanged = writeReply.events.find(
    (e: any) => "FsChanged" in e,
  );
  expect(fsChanged).toBeTruthy();
  expect(fsChanged.FsChanged).toBe("/workspace/a.txt");

  // ReadFile
  const readReply = await send(page, {
    type: "ReadFile",
    path: "/workspace/a.txt",
  });

  const stdoutEvt = readReply.events.find(
    (e: any) => "Stdout" in e,
  );
  expect(stdoutEvt).toBeTruthy();
  const text = new TextDecoder().decode(new Uint8Array(stdoutEvt.Stdout));
  expect(text).toBe("alpha");
});

test("ListDir includes written file", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  // Write a file first
  await send(page, {
    type: "WriteFile",
    path: "/workspace/a.txt",
    data: Array.from(new TextEncoder().encode("alpha")),
  });

  // ListDir
  const listReply = await send(page, {
    type: "ListDir",
    path: "/workspace",
  });

  const stdoutEvt = listReply.events.find(
    (e: any) => "Stdout" in e,
  );
  expect(stdoutEvt).toBeTruthy();
  const listing = new TextDecoder().decode(
    new Uint8Array(stdoutEvt.Stdout),
  );
  expect(listing).toContain("a.txt");
});

test("Run with in-shell redirect emits FsChanged", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  const runReply = await send(page, {
    type: "Run",
    input: "echo hi > /workspace/b.txt",
  });

  const fsChanged = runReply.events.filter((e: any) => "FsChanged" in e);
  expect(fsChanged.length).toBe(1);
  expect(fsChanged[0].FsChanged).toBe("/workspace/b.txt");

  const exitEvt = runReply.events.find((e: any) => "Exit" in e);
  expect(exitEvt.Exit).toBe(0);

  // Read-back confirms the write really happened.
  const readReply = await send(page, {
    type: "ReadFile",
    path: "/workspace/b.txt",
  });
  const stdoutEvt = readReply.events.find((e: any) => "Stdout" in e);
  const text = new TextDecoder().decode(new Uint8Array(stdoutEvt.Stdout));
  expect(text).toBe("hi\n");
});

test("Run with mkdir/touch/rm emits one FsChanged per path", async ({
  page,
}) => {
  await page.goto("/");
  await initWorker(page);

  const runReply = await send(page, {
    type: "Run",
    input:
      "mkdir /workspace/d\ntouch /workspace/d/f.txt\nrm /workspace/d/f.txt",
  });

  const changed = runReply.events
    .filter((e: any) => "FsChanged" in e)
    .map((e: any) => e.FsChanged);

  expect(changed).toContain("/workspace/d");
  expect(changed).toContain("/workspace/d/f.txt");

  const exitEvt = runReply.events.find((e: any) => "Exit" in e);
  expect(exitEvt.Exit).toBe(0);
});

test("Run cat reads VFS file written via WriteFile", async ({ page }) => {
  await page.goto("/");
  await initWorker(page);

  // Write via protocol
  await send(page, {
    type: "WriteFile",
    path: "/workspace/a.txt",
    data: Array.from(new TextEncoder().encode("alpha")),
  });

  // Read via shell command
  const runReply = await send(page, {
    type: "Run",
    input: "cat /workspace/a.txt",
  });

  const stdoutEvt = runReply.events.find(
    (e: any) => "Stdout" in e,
  );
  expect(stdoutEvt).toBeTruthy();
  const text = new TextDecoder().decode(new Uint8Array(stdoutEvt.Stdout));
  expect(text).toBe("alpha");

  // Should exit 0
  const exitEvt = runReply.events.find((e: any) => "Exit" in e);
  expect(exitEvt).toBeTruthy();
  expect(exitEvt.Exit).toBe(0);
});
