import assert from "node:assert/strict";

import { createRunner } from "../src/runner-main.mjs";

const iterations = Number(process.env.WASMSH_RESTORE_BENCH_ITERATIONS ?? 3);
const strictThresholdMs = process.env.WASMSH_RESTORE_STRICT_MS
  ? Number(process.env.WASMSH_RESTORE_STRICT_MS)
  : null;

const runner = await createRunner();
try {
  for (let index = 0; index < iterations; index += 1) {
    const session = await runner.createSession();
    await session.close();
  }
  const metrics = runner.metrics.snapshot();
  const p95 = metrics.wasmsh_session_restore_duration_ms.p95;
  if (strictThresholdMs !== null) {
    assert.ok(
      p95 <= strictThresholdMs,
      `restore p95 ${p95}ms exceeds strict threshold ${strictThresholdMs}ms`,
    );
  }
  process.stdout.write(`${JSON.stringify({ ok: true, p95 })}\n`);
} finally {
  await runner.close();
}
