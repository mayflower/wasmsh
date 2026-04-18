import { describe, it } from "node:test";
import { existsSync, readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

import {
  assertThresholds,
  runSessionBenchmark,
  writeBenchmarkReport,
} from "../lib/perf-harness.mjs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ASSETS_DIR = resolve(__dirname, "../../../packages/npm/wasmsh-pyodide/assets");
const OUTPUT_PATH = resolve(__dirname, "../tmp/perf-harness-report.json");
const SKIP =
  process.env.SKIP_PYODIDE === "1" ||
  !existsSync(resolve(ASSETS_DIR, "pyodide.asm.wasm"));

describe("pyodide node perf harness", () => {
  it("collects a structured benchmark report", { skip: SKIP, timeout: 180_000 }, async () => {
    const report = await runSessionBenchmark({
      assetDir: ASSETS_DIR,
      warmup: 0,
      samples: 1,
      concurrency: 2,
    });

    assert.equal(report.suite, "pyodide-node-session");
    assert.equal(report.measurements.length, 2);
    assert.equal(report.concurrency, 2);
    assert.equal(report.totalSessions, 2);
    assert.equal(report.metrics.initMs.count, 2);
    assert.equal(report.batchMetrics.batchDurationMs.count, 1);
    assert.equal(report.batches.length, 1);

    for (const sample of report.measurements) {
      assert.equal(sample.checks.shellExitCode, 0);
      assert.equal(sample.checks.pythonExitCode, 0);
      assert.equal(sample.checks.steadyStateExitCode, 0);
      assert.match(sample.checks.shellStdout, /^ready\s*$/);
      assert.match(sample.checks.pythonStdout, /^42\s*$/);
      assert.match(sample.checks.steadyStateStdout, /^499500\s*$/);
      assert.ok(sample.initMs > 0);
      assert.ok(sample.totalMs >= sample.initMs);
    }

    const writtenPath = writeBenchmarkReport(report, OUTPUT_PATH);
    const written = JSON.parse(readFileSync(writtenPath, "utf-8"));
    assert.equal(written.measurements.length, 2);
    assert.equal(written.metrics.firstPythonMs.count, 2);
    assert.equal(written.batchMetrics.batchDurationMs.count, 1);
  });

  it("enforces optional thresholds when requested", () => {
    assert.throws(
      () => assertThresholds({
        metrics: {
          initMs: { p95: 150 },
          firstPythonMs: { p95: 80 },
          totalMs: { p95: 260 },
        },
      }, {
        initMs: 100,
      }),
      /initMs p95 150.00ms exceeded 100.00ms/,
    );
  });
});
