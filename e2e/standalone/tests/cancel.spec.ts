import { test, expect } from "@playwright/test";

/**
 * Cancel / resource-limit tests.
 *
 * Because wasm execution in a Web Worker is synchronous, we cannot send
 * Cancel while a command is running. Instead we test two things:
 *
 * 1. A tight loop with a small step budget is killed by the resource
 *    limiter — proving enforcement works in the browser build.
 *
 * 2. The Cancel command itself returns the expected diagnostic shape,
 *    confirming the protocol plumbing end-to-end.
 */

async function initWorker(page: any, stepBudget: number) {
  return page.evaluate(
    async (budget: number) => {
      const client = (window as any).createShellWorkerClient(15_000);
      (window as any)._send = (msg: any) => client.send(msg);

      const initReply = await client.send({
        type: "Init",
        step_budget: budget,
      });
      return initReply;
    },
    stepBudget,
  );
}

function send(page: any, msg: any): Promise<any> {
  return page.evaluate(async (m: any) => {
    const send = (window as any)._send;
    return send(m);
  }, msg);
}

// --------------------------------------------------------------------------

test("step budget kills an infinite loop in the browser worker", async ({
  page,
}) => {
  await page.goto("/");
  // Small budget: enough for init + a few iterations, not enough for ∞.
  await initWorker(page, 200);

  const reply = await send(page, {
    type: "Run",
    input: "while true; do :; done",
  });

  // The loop must NOT run forever; we must get events back.
  expect(reply.events).toBeTruthy();
  expect(reply.events.length).toBeGreaterThan(0);

  // Should get a non-zero exit code (resource exhaustion).
  const exitEvt = reply.events.find((e: any) => "Exit" in e);
  expect(exitEvt).toBeTruthy();
  expect(exitEvt.Exit).not.toBe(0);
});

test("Cancel returns Diagnostic Info event", async ({ page }) => {
  await page.goto("/");
  await initWorker(page, 0);

  const reply = await send(page, { type: "Cancel" });

  expect(reply.events).toBeTruthy();
  expect(reply.events.length).toBeGreaterThan(0);

  // Should be Diagnostic(Info, "cancel received")
  const diag = reply.events.find((e: any) => "Diagnostic" in e);
  expect(diag).toBeTruthy();
  expect(diag.Diagnostic[0]).toBe("Info");
  expect(diag.Diagnostic[1]).toContain("cancel");
});
