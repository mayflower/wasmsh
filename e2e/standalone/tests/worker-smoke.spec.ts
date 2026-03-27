import { test, expect } from "@playwright/test";

/**
 * Smoke test: loads wasmsh in a real Web Worker, sends Init + Run,
 * and asserts on the protocol events returned.
 */
test("worker returns Version, stdout, and Exit(0) for echo hello", async ({
  page,
}) => {
  await page.goto("/");

  // Ask the page to spin up a worker, send Init, then Run "echo hello",
  // and collect all events from both responses.
  const result = await page.evaluate(async () => {
    const worker = (window as any).createShellWorker();

    function sendAndReceive(msg: any): Promise<any> {
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error("worker timeout")), 10_000);
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

    // 1. Init
    const initReply = await sendAndReceive({
      type: "Init",
      step_budget: 100000,
    });

    // 2. Run
    const runReply = await sendAndReceive({
      type: "Run",
      input: "echo hello",
    });

    return { initEvents: initReply.events, runEvents: runReply.events };
  });

  // -- Assertions --

  // Init must return a Version event with the protocol version string
  const versionEvt = result.initEvents.find(
    (e: any) => "Version" in e
  );
  expect(versionEvt).toBeTruthy();
  expect(versionEvt.Version).toBe("0.1.0");

  // Run must return Stdout with "hello\n"
  const stdoutEvt = result.runEvents.find((e: any) => "Stdout" in e);
  expect(stdoutEvt).toBeTruthy();
  const stdoutText = new TextDecoder().decode(
    new Uint8Array(stdoutEvt.Stdout)
  );
  expect(stdoutText).toBe("hello\n");

  // Run must return Exit(0)
  const exitEvt = result.runEvents.find((e: any) => "Exit" in e);
  expect(exitEvt).toBeTruthy();
  expect(exitEvt.Exit).toBe(0);
});
