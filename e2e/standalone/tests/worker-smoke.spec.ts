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
    const client = (window as any).createShellWorkerClient(10_000);

    // 1. Init
    const initReply = await client.send({
      type: "Init",
      step_budget: 100000,
    });

    // 2. Run
    const runReply = await client.send({
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
